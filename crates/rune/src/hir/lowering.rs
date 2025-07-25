use core::mem::{replace, take};
use core::ops::Neg;

use num::ToPrimitive;
use tracing::instrument_ast;

use crate::alloc::prelude::*;
use crate::alloc::try_format;
use crate::alloc::{self, Box, HashMap, HashSet};
use crate::ast::{self, NumberSize, Spanned};
use crate::compile::meta;
use crate::compile::{self, ErrorKind, WithSpan};
use crate::hash::ParametersBuilder;
use crate::hir;
use crate::parse::Resolve;
use crate::query::AsyncBlock;
use crate::query::Closure;
use crate::query::SecondaryBuildEntry;
use crate::query::{self, GenericsParameters, Named, SecondaryBuild};
use crate::runtime::{self, ConstInstance, ConstValue, ConstValueKind, Inline, Type, TypeHash};
use crate::{Hash, Item};

use super::{Ctxt, Needs};

/// Lower an empty function.
#[instrument_ast(span = span)]
pub(crate) fn empty_fn<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::EmptyBlock,
    span: &dyn Spanned,
) -> compile::Result<hir::ItemFn<'hir>> {
    Ok(hir::ItemFn {
        span: span.span(),
        args: &[],
        body: statements(cx, None, &ast.statements, span)?,
    })
}

/// Lower a function item.
#[instrument_ast(span = ast)]
pub(crate) fn item_fn<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::ItemFn,
) -> compile::Result<hir::ItemFn<'hir>> {
    alloc_with!(cx, ast);

    Ok(hir::ItemFn {
        span: ast.span(),
        args: iter!(&ast.args, |(ast, _)| fn_arg(cx, ast)?),
        body: block(cx, None, &ast.body)?,
    })
}

/// Assemble a closure expression.
#[instrument_ast(span = ast)]
fn expr_call_closure<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::ExprClosure,
) -> compile::Result<hir::ExprKind<'hir>> {
    alloc_with!(cx, ast);

    let item =
        cx.q.item_for("lowering closure call", ast.id)
            .with_span(ast)?;

    let Some(meta) = cx.q.query_meta(ast, item.item, Default::default())? else {
        return Err(compile::Error::new(
            ast,
            ErrorKind::MissingItem {
                item: cx.q.pool.item(item.item).try_to_owned()?,
            },
        ));
    };

    let meta::Kind::Closure { call, do_move, .. } = meta.kind else {
        return Err(compile::Error::expected_meta(
            ast,
            meta.info(cx.q.pool)?,
            "a closure",
        ));
    };

    tracing::trace!("queuing closure build entry");

    cx.scopes.push_captures()?;

    let args = iter!(ast.args.as_slice(), |(arg, _)| fn_arg(cx, arg)?);
    let body = alloc!(expr(cx, &ast.body)?);

    let layer = cx.scopes.pop().with_span(&ast.body)?;

    cx.q.set_used(&meta.item_meta)?;

    let captures = &*iter!(layer.captures().map(|(_, id)| id));

    let Some(queue) = cx.secondary_builds.as_mut() else {
        return Err(compile::Error::new(ast, ErrorKind::ClosureInConst));
    };

    queue.try_push(SecondaryBuildEntry {
        item_meta: meta.item_meta,
        build: SecondaryBuild::Closure(Closure {
            hir: alloc!(hir::ExprClosure {
                args,
                body,
                captures,
            }),
            call,
        }),
    })?;

    if captures.is_empty() {
        return Ok(hir::ExprKind::Fn(meta.hash));
    }

    Ok(hir::ExprKind::CallClosure(alloc!(hir::ExprCallClosure {
        hash: meta.hash,
        do_move,
        captures,
    })))
}

#[inline]
pub(crate) fn block<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    label: Option<&(ast::Label, T![:])>,
    ast: &ast::Block,
) -> compile::Result<hir::Block<'hir>> {
    statements(cx, label, &ast.statements, ast)
}

#[instrument_ast(span = span)]
fn statements<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    label: Option<&(ast::Label, T![:])>,
    statements: &[ast::Stmt],
    span: &dyn Spanned,
) -> compile::Result<hir::Block<'hir>> {
    alloc_with!(cx, span);

    let label = match label {
        Some((label, _)) => Some(alloc_str!(label.resolve(resolve_context!(cx.q))?)),
        None => None,
    };

    cx.scopes.push(label)?;

    let at = cx.statements.len();

    let mut value = None;

    for ast in statements {
        let last = match ast {
            ast::Stmt::Local(ast) => {
                let depacked = if ast.attributes.is_empty() && cx.q.options.lowering > 0 {
                    unpack_locals(cx, &ast.pat, &ast.expr)?
                } else {
                    false
                };

                if !depacked {
                    let stmt = hir::Stmt::Local(alloc!(local(cx, ast)?));
                    cx.statement_buffer.try_push(stmt)?;
                }

                value.take()
            }
            ast::Stmt::Expr(ast) => {
                if let Some(stmt) = value.replace(&*alloc!(expr(cx, ast)?)).map(hir::Stmt::Expr) {
                    cx.statement_buffer.try_push(stmt)?;
                }

                None
            }
            ast::Stmt::Semi(ast) => {
                let stmt = hir::Stmt::Expr(alloc!(expr(cx, &ast.expr)?));
                cx.statement_buffer.try_push(stmt)?;
                value.take()
            }
            ast::Stmt::Item(..) => continue,
        };

        if let Some(last) = last {
            cx.statements
                .try_push(hir::Stmt::Expr(last))
                .with_span(span)?;
        }

        for stmt in cx.statement_buffer.drain(..) {
            cx.statements.try_push(stmt).with_span(span)?;
        }
    }

    let statements = iter!(cx.statements.drain(at..));

    let layer = cx.scopes.pop().with_span(span)?;

    Ok(hir::Block {
        span: span.span(),
        label,
        statements,
        value,
        drop: iter!(layer.into_drop_order()),
    })
}

#[instrument_ast(span = ast)]
fn expr_range<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::ExprRange,
) -> compile::Result<hir::ExprRange<'hir>> {
    match (ast.start.as_deref(), ast.end.as_deref(), &ast.limits) {
        (Some(start), None, ast::ExprRangeLimits::HalfOpen(..)) => Ok(hir::ExprRange::RangeFrom {
            start: expr(cx, start)?,
        }),
        (None, None, ast::ExprRangeLimits::HalfOpen(..)) => Ok(hir::ExprRange::RangeFull),
        (Some(start), Some(end), ast::ExprRangeLimits::Closed(..)) => {
            Ok(hir::ExprRange::RangeInclusive {
                start: expr(cx, start)?,
                end: expr(cx, end)?,
            })
        }
        (None, Some(end), ast::ExprRangeLimits::Closed(..)) => {
            Ok(hir::ExprRange::RangeToInclusive {
                end: expr(cx, end)?,
            })
        }
        (None, Some(end), ast::ExprRangeLimits::HalfOpen(..)) => Ok(hir::ExprRange::RangeTo {
            end: expr(cx, end)?,
        }),
        (Some(start), Some(end), ast::ExprRangeLimits::HalfOpen(..)) => Ok(hir::ExprRange::Range {
            start: expr(cx, start)?,
            end: expr(cx, end)?,
        }),
        (Some(..) | None, None, ast::ExprRangeLimits::Closed(..)) => Err(compile::Error::msg(
            ast,
            "Unsupported range, you probably want `..` instead of `..=`",
        )),
    }
}

#[instrument_ast(span = ast)]
fn expr_object<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::ExprObject,
) -> compile::Result<hir::ExprKind<'hir>> {
    alloc_with!(cx, ast);

    let span = ast;
    let mut keys_dup = HashMap::new();

    let assignments = &mut *iter!(&ast.assignments, |(ast, _)| {
        let key = object_key(cx, &ast.key)?;

        if let Some(_existing) = keys_dup.try_insert(key.1, key.0)? {
            return Err(compile::Error::new(
                key.0,
                ErrorKind::DuplicateObjectKey {
                    #[cfg(feature = "emit")]
                    existing: _existing.span(),
                    #[cfg(feature = "emit")]
                    object: key.0.span(),
                },
            ));
        }

        let assign = match &ast.assign {
            Some((_, ast)) => expr(cx, ast)?,
            None => {
                let Some((name, _)) = cx.scopes.get(hir::Name::Str(key.1))? else {
                    return Err(compile::Error::new(
                        key.0,
                        ErrorKind::MissingLocal {
                            name: key.1.try_to_string()?.try_into()?,
                        },
                    ));
                };

                hir::Expr {
                    span: ast.span(),
                    kind: hir::ExprKind::Variable(name),
                }
            }
        };

        hir::FieldAssign {
            key: (key.0.span(), key.1),
            assign,
            position: None,
        }
    });

    let mut check_object_fields = |fields: &[meta::FieldMeta], item: &Item| {
        let mut named = HashMap::new();

        for f in fields.iter() {
            named.try_insert(f.name.as_ref(), f)?;
        }

        for assign in assignments.iter_mut() {
            match named.remove(assign.key.1) {
                Some(field_meta) => {
                    assign.position = Some(field_meta.position);
                }
                None => {
                    return Err(compile::Error::new(
                        assign.key.0,
                        ErrorKind::LitObjectNotField {
                            field: assign.key.1.try_into()?,
                            item: item.try_to_owned()?,
                        },
                    ));
                }
            };
        }

        if let Some(field) = named.into_keys().next() {
            return Err(compile::Error::new(
                span,
                ErrorKind::LitObjectMissingField {
                    field: field.try_into()?,
                    item: item.try_to_owned()?,
                },
            ));
        }

        Ok(())
    };

    let kind = match &ast.ident {
        ast::ObjectIdent::Named(path) => {
            let named = cx.q.convert_path(path)?;
            let parameters = generics_parameters(cx, &named)?;
            let meta = cx.lookup_meta(path, named.item, parameters)?;
            let item = cx.q.pool.item(meta.item_meta.item);

            match &meta.kind {
                meta::Kind::Struct {
                    fields: meta::Fields::Empty,
                    constructor,
                    ..
                } => {
                    check_object_fields(&[], item)?;

                    match constructor {
                        Some(_) => hir::ExprObjectKind::ExternalType {
                            hash: meta.hash,
                            args: 0,
                        },
                        None => hir::ExprObjectKind::Struct { hash: meta.hash },
                    }
                }
                meta::Kind::Struct {
                    fields: meta::Fields::Named(st),
                    constructor,
                    ..
                } => {
                    check_object_fields(&st.fields, item)?;

                    match constructor {
                        Some(_) => hir::ExprObjectKind::ExternalType {
                            hash: meta.hash,
                            args: st.fields.len(),
                        },
                        None => hir::ExprObjectKind::Struct { hash: meta.hash },
                    }
                }
                _ => {
                    return Err(compile::Error::new(
                        span,
                        ErrorKind::UnsupportedLitObject {
                            meta: meta.info(cx.q.pool)?,
                        },
                    ));
                }
            }
        }
        ast::ObjectIdent::Anonymous(..) => hir::ExprObjectKind::Anonymous,
    };

    Ok(hir::ExprKind::Object(alloc!(hir::ExprObject {
        kind,
        assignments,
    })))
}

/// Lower an expression.
#[instrument_ast(span = ast)]
pub(crate) fn expr<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::Expr,
) -> compile::Result<hir::Expr<'hir>> {
    alloc_with!(cx, ast);

    let in_path = take(&mut cx.in_path);

    let kind = match ast {
        ast::Expr::Path(ast) => expr_path(cx, ast, in_path)?,
        ast::Expr::Assign(ast) => hir::ExprKind::Assign(alloc!(hir::ExprAssign {
            lhs: expr(cx, &ast.lhs)?,
            rhs: expr(cx, &ast.rhs)?,
        })),
        // TODO: lower all of these loop constructs to the same loop-like
        // representation. We only do different ones here right now since it's
        // easier when refactoring.
        ast::Expr::While(ast) => {
            let label = match &ast.label {
                Some((label, _)) => Some(alloc_str!(label.resolve(resolve_context!(cx.q))?)),
                None => None,
            };

            cx.scopes.push_loop(label)?;
            let condition = condition(cx, &ast.condition)?;
            let body = block(cx, None, &ast.body)?;
            let layer = cx.scopes.pop().with_span(ast)?;

            hir::ExprKind::Loop(alloc!(hir::ExprLoop {
                label,
                condition: Some(alloc!(condition)),
                body,
                drop: iter!(layer.into_drop_order()),
            }))
        }
        ast::Expr::Loop(ast) => {
            let label = match &ast.label {
                Some((label, _)) => Some(alloc_str!(label.resolve(resolve_context!(cx.q))?)),
                None => None,
            };

            cx.scopes.push_loop(label)?;
            let body = block(cx, None, &ast.body)?;
            let layer = cx.scopes.pop().with_span(ast)?;

            let kind = hir::ExprKind::Loop(alloc!(hir::ExprLoop {
                label,
                condition: None,
                body,
                drop: iter!(layer.into_drop_order()),
            }));

            kind
        }
        ast::Expr::For(ast) => {
            let iter = expr(cx, &ast.iter)?;

            let label = match &ast.label {
                Some((label, _)) => Some(alloc_str!(label.resolve(resolve_context!(cx.q))?)),
                None => None,
            };

            cx.scopes.push_loop(label)?;
            let binding = pat_binding(cx, &ast.binding)?;
            let body = block(cx, None, &ast.body)?;

            let layer = cx.scopes.pop().with_span(ast)?;

            hir::ExprKind::For(alloc!(hir::ExprFor {
                label,
                binding,
                iter,
                body,
                drop: iter!(layer.into_drop_order()),
            }))
        }
        ast::Expr::Let(ast) => hir::ExprKind::Let(alloc!(hir::ExprLet {
            pat: pat_binding(cx, &ast.pat)?,
            expr: expr(cx, &ast.expr)?,
        })),
        ast::Expr::If(ast) => hir::ExprKind::If(alloc!(expr_if(cx, ast)?)),
        ast::Expr::Match(ast) => hir::ExprKind::Match(alloc!(hir::ExprMatch {
            expr: alloc!(expr(cx, &ast.expr)?),
            branches: iter!(&ast.branches, |(ast, _)| {
                cx.scopes.push(None)?;

                let pat = pat_binding(cx, &ast.pat)?;
                let condition = option!(&ast.condition, |(_, ast)| expr(cx, ast)?);
                let body = expr(cx, &ast.body)?;

                let layer = cx.scopes.pop().with_span(ast)?;

                hir::ExprMatchBranch {
                    span: ast.span(),
                    pat,
                    condition,
                    body,
                    drop: iter!(layer.into_drop_order()),
                }
            }),
        })),
        ast::Expr::Call(ast) => hir::ExprKind::Call(alloc!(expr_call(cx, ast)?)),
        ast::Expr::FieldAccess(ast) => {
            hir::ExprKind::FieldAccess(alloc!(expr_field_access(cx, ast)?))
        }
        ast::Expr::Empty(ast) => {
            // NB: restore in_path setting.
            cx.in_path = in_path;
            hir::ExprKind::Group(alloc!(expr(cx, &ast.expr)?))
        }
        ast::Expr::Binary(ast) => {
            let rhs_needs = match &ast.op {
                ast::BinOp::As(..) | ast::BinOp::Is(..) | ast::BinOp::IsNot(..) => Needs::Type,
                _ => Needs::Value,
            };

            let lhs = expr(cx, &ast.lhs)?;

            let needs = replace(&mut cx.needs, rhs_needs);
            let rhs = expr(cx, &ast.rhs)?;
            cx.needs = needs;

            hir::ExprKind::Binary(alloc!(hir::ExprBinary {
                lhs,
                op: ast.op,
                rhs,
            }))
        }
        ast::Expr::Unary(ast) => expr_unary(cx, ast)?,
        ast::Expr::Index(ast) => hir::ExprKind::Index(alloc!(hir::ExprIndex {
            target: expr(cx, &ast.target)?,
            index: expr(cx, &ast.index)?,
        })),
        ast::Expr::Block(ast) => expr_block(cx, ast)?,
        ast::Expr::Break(ast) => hir::ExprKind::Break(alloc!(expr_break(cx, ast)?)),
        ast::Expr::Continue(ast) => hir::ExprKind::Continue(alloc!(expr_continue(cx, ast)?)),
        ast::Expr::Yield(ast) => hir::ExprKind::Yield(option!(&ast.expr, |ast| expr(cx, ast)?)),
        ast::Expr::Return(ast) => hir::ExprKind::Return(option!(&ast.expr, |ast| expr(cx, ast)?)),
        ast::Expr::Await(ast) => hir::ExprKind::Await(alloc!(expr(cx, &ast.expr)?)),
        ast::Expr::Try(ast) => hir::ExprKind::Try(alloc!(expr(cx, &ast.expr)?)),
        ast::Expr::Select(ast) => {
            let mut default = None;
            let mut branches = Vec::new();
            let mut exprs = Vec::new();

            for (ast, _) in &ast.branches {
                match ast {
                    ast::ExprSelectBranch::Pat(ast) => {
                        cx.scopes.push(None)?;

                        let pat = pat_binding(cx, &ast.pat)?;
                        let body = expr(cx, &ast.body)?;

                        let layer = cx.scopes.pop().with_span(&ast)?;

                        exprs.try_push(expr(cx, &ast.expr)?).with_span(&ast.expr)?;

                        branches.try_push(hir::ExprSelectBranch {
                            pat,
                            body,
                            drop: iter!(layer.into_drop_order()),
                        })?;
                    }
                    ast::ExprSelectBranch::Default(ast) => {
                        if default.is_some() {
                            return Err(compile::Error::new(
                                ast,
                                ErrorKind::SelectMultipleDefaults,
                            ));
                        }

                        default = Some(alloc!(expr(cx, &ast.body)?));
                    }
                }
            }

            hir::ExprKind::Select(alloc!(hir::ExprSelect {
                branches: iter!(branches),
                exprs: iter!(exprs),
                default: option!(default),
            }))
        }
        ast::Expr::Closure(ast) => expr_call_closure(cx, ast)?,
        ast::Expr::Lit(ast) => hir::ExprKind::Lit(lit(cx, &ast.lit)?),
        ast::Expr::Object(ast) => expr_object(cx, ast)?,
        ast::Expr::Tuple(ast) => hir::ExprKind::Tuple(alloc!(hir::ExprSeq {
            items: iter!(&ast.items, |(ast, _)| expr(cx, ast)?),
        })),
        ast::Expr::Vec(ast) => hir::ExprKind::Vec(alloc!(hir::ExprSeq {
            items: iter!(&ast.items, |(ast, _)| expr(cx, ast)?),
        })),
        ast::Expr::Range(ast) => hir::ExprKind::Range(alloc!(expr_range(cx, ast)?)),
        ast::Expr::Group(ast) => hir::ExprKind::Group(alloc!(expr(cx, &ast.expr)?)),
        ast::Expr::MacroCall(ast) => {
            let Some(id) = ast.id else {
                return Err(compile::Error::msg(ast, "missing expanded macro id"));
            };

            match cx.q.builtin_macro_for(id).with_span(ast)?.as_ref() {
                query::BuiltInMacro::Template(ast) => {
                    let old = replace(&mut cx.in_template, true);

                    let result = hir::ExprKind::Template(alloc!(hir::BuiltInTemplate {
                        span: ast.span,
                        from_literal: ast.from_literal,
                        exprs: iter!(&ast.exprs, |ast| expr(cx, ast)?),
                    }));

                    cx.in_template = old;
                    result
                }
                query::BuiltInMacro::Format(ast) => {
                    let spec = hir::BuiltInFormatSpec {
                        fill: ast.fill,
                        align: ast.align,
                        width: ast.width,
                        precision: ast.precision,
                        flags: ast.flags,
                        format_type: ast.format_type,
                    };

                    hir::ExprKind::Format(alloc!(hir::BuiltInFormat {
                        spec,
                        value: alloc!(expr(cx, &ast.value)?),
                    }))
                }
                query::BuiltInMacro::File(ast) => hir::ExprKind::Lit(lit(cx, &ast.value)?),
                query::BuiltInMacro::Line(ast) => hir::ExprKind::Lit(lit(cx, &ast.value)?),
            }
        }
    };

    Ok(hir::Expr {
        span: ast.span(),
        kind,
    })
}

/// Construct a pattern from a constant value.
#[instrument_ast(span = span)]
fn pat_const_value<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    const_value: &ConstValue,
    span: &dyn Spanned,
) -> compile::Result<hir::Pat<'hir>> {
    alloc_with!(cx, span);

    let kind = 'kind: {
        let lit = match const_value.as_kind() {
            ConstValueKind::Inline(value) => match *value {
                Inline::Unit => {
                    break 'kind hir::PatKind::Sequence(alloc!(hir::PatSequence {
                        kind: hir::PatSequenceKind::Sequence {
                            hash: runtime::Tuple::HASH,
                            count: 0,
                            is_open: false,
                        },
                        items: &[],
                    }));
                }
                Inline::Bool(b) => hir::Lit::Bool(b),
                Inline::Char(ch) => hir::Lit::Char(ch),
                Inline::Unsigned(integer) => hir::Lit::Unsigned(integer),
                Inline::Signed(integer) => hir::Lit::Signed(integer),
                _ => {
                    return Err(compile::Error::msg(
                        span,
                        "Unsupported constant value in pattern",
                    ))
                }
            },
            ConstValueKind::String(string) => hir::Lit::Str(alloc_str!(string.as_ref())),
            ConstValueKind::Bytes(bytes) => hir::Lit::ByteStr(alloc_bytes!(bytes.as_ref())),
            ConstValueKind::Instance(instance) => match &**instance {
                ConstInstance {
                    hash: runtime::Vec::HASH,
                    variant_hash: Hash::EMPTY,
                    fields,
                } => {
                    let items = iter!(fields.iter(), fields.len(), |value| pat_const_value(
                        cx, value, span
                    )?);

                    break 'kind hir::PatKind::Sequence(alloc!(hir::PatSequence {
                        kind: hir::PatSequenceKind::Sequence {
                            hash: runtime::Vec::HASH,
                            count: items.len(),
                            is_open: false,
                        },
                        items,
                    }));
                }
                ConstInstance {
                    hash: runtime::OwnedTuple::HASH,
                    variant_hash: Hash::EMPTY,
                    fields,
                } => {
                    let items = iter!(fields.iter(), fields.len(), |value| pat_const_value(
                        cx, value, span
                    )?);

                    break 'kind hir::PatKind::Sequence(alloc!(hir::PatSequence {
                        kind: hir::PatSequenceKind::Sequence {
                            hash: runtime::Vec::HASH,
                            count: items.len(),
                            is_open: false,
                        },
                        items,
                    }));
                }
                ConstInstance {
                    hash: runtime::Object::HASH,
                    variant_hash: Hash::EMPTY,
                    fields,
                } => {
                    let bindings = iter!(fields.iter(), fields.len(), |value| {
                        let (key, value) = value.as_pair().with_span(span)?;
                        let key = key.as_string().with_span(span)?;
                        let pat = alloc!(pat_const_value(cx, value, span)?);
                        hir::Binding::Binding(span.span(), alloc_str!(key.as_ref()), pat)
                    });

                    break 'kind hir::PatKind::Object(alloc!(hir::PatObject {
                        kind: hir::PatSequenceKind::Sequence {
                            hash: runtime::Object::HASH,
                            count: bindings.len(),
                            is_open: false,
                        },
                        bindings,
                    }));
                }
                _ => {
                    return Err(compile::Error::msg(
                        span,
                        "Unsupported constant value in pattern",
                    ));
                }
            },
        };

        hir::PatKind::Lit(alloc!(hir::Expr {
            span: span.span(),
            kind: hir::ExprKind::Lit(lit),
        }))
    };

    Ok(hir::Pat {
        span: span.span(),
        kind,
    })
}

#[instrument_ast(span = ast)]
fn expr_if<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::ExprIf,
) -> compile::Result<hir::Conditional<'hir>> {
    alloc_with!(cx, ast);

    let length = 1 + ast.expr_else_ifs.len();

    let then = [(
        ast.if_.span().join(ast.block.span()),
        &ast.condition,
        &ast.block,
    )]
    .into_iter();

    let else_ifs = ast
        .expr_else_ifs
        .iter()
        .map(|ast| (ast.span(), &ast.condition, &ast.block));

    let branches = iter!(then.chain(else_ifs), length, |(span, c, b)| {
        cx.scopes.push(None)?;

        let condition = condition(cx, c)?;
        let block = block(cx, None, b)?;

        let layer = cx.scopes.pop().with_span(ast)?;

        let condition = &*alloc!(condition);
        let drop = &*iter!(layer.into_drop_order());

        hir::ConditionalBranch {
            span,
            condition,
            block,
            drop,
        }
    });

    let fallback = match &ast.expr_else {
        Some(ast) => Some(&*alloc!(block(cx, None, &ast.block)?)),
        None => None,
    };

    Ok(hir::Conditional { branches, fallback })
}

#[instrument_ast(span = ast)]
fn lit<'hir>(cx: &mut Ctxt<'hir, '_, '_>, ast: &ast::Lit) -> compile::Result<hir::Lit<'hir>> {
    alloc_with!(cx, ast);

    match ast {
        ast::Lit::Bool(lit) => Ok(hir::Lit::Bool(lit.value)),
        ast::Lit::Number(lit) => {
            let n = lit.resolve(resolve_context!(cx.q))?;

            match (n.value, n.suffix) {
                (ast::NumberValue::Float(n), _) => Ok(hir::Lit::Float(n)),
                (ast::NumberValue::Integer(int), Some(ast::NumberSuffix::Unsigned(_, size))) => {
                    let Some(n) = int.to_u64() else {
                        return Err(compile::Error::new(
                            ast,
                            ErrorKind::BadUnsignedOutOfBounds { size },
                        ));
                    };

                    if !size.unsigned_in(n) {
                        return Err(compile::Error::new(
                            ast,
                            ErrorKind::BadUnsignedOutOfBounds { size },
                        ));
                    }

                    Ok(hir::Lit::Unsigned(n))
                }
                (ast::NumberValue::Integer(int), Some(ast::NumberSuffix::Signed(_, size))) => {
                    let Some(n) = int.to_i64() else {
                        return Err(compile::Error::new(
                            ast,
                            ErrorKind::BadSignedOutOfBounds { size },
                        ));
                    };

                    if !size.signed_in(n) {
                        return Err(compile::Error::new(
                            ast,
                            ErrorKind::BadSignedOutOfBounds { size },
                        ));
                    }

                    Ok(hir::Lit::Signed(n))
                }
                (ast::NumberValue::Integer(int), _) => {
                    let Some(n) = int.to_i64() else {
                        return Err(compile::Error::new(
                            ast,
                            ErrorKind::BadSignedOutOfBounds {
                                size: NumberSize::S64,
                            },
                        ));
                    };

                    Ok(hir::Lit::Signed(n))
                }
            }
        }
        ast::Lit::Byte(lit) => {
            let b = lit.resolve(resolve_context!(cx.q))?;
            Ok(hir::Lit::Unsigned(b as u64))
        }
        ast::Lit::Char(lit) => {
            let ch = lit.resolve(resolve_context!(cx.q))?;
            Ok(hir::Lit::Char(ch))
        }
        ast::Lit::Str(lit) => {
            let string = if cx.in_template {
                lit.resolve_template_string(resolve_context!(cx.q))?
            } else {
                lit.resolve_string(resolve_context!(cx.q))?
            };

            Ok(hir::Lit::Str(alloc_str!(string.as_ref())))
        }
        ast::Lit::ByteStr(lit) => {
            let bytes = lit.resolve(resolve_context!(cx.q))?;
            Ok(hir::Lit::ByteStr(alloc_bytes!(bytes.as_ref())))
        }
    }
}

#[instrument_ast(span = ast)]
fn expr_unary<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::ExprUnary,
) -> compile::Result<hir::ExprKind<'hir>> {
    alloc_with!(cx, ast);

    // NB: special unary expressions.
    if let ast::UnOp::BorrowRef { .. } = ast.op {
        return Err(compile::Error::new(ast, ErrorKind::UnsupportedRef));
    }

    let (
        ast::UnOp::Neg(..),
        ast::Expr::Lit(ast::ExprLit {
            lit: ast::Lit::Number(n),
            ..
        }),
    ) = (ast.op, &*ast.expr)
    else {
        return Ok(hir::ExprKind::Unary(alloc!(hir::ExprUnary {
            op: ast.op,
            expr: expr(cx, &ast.expr)?,
        })));
    };

    let number = n.resolve(resolve_context!(cx.q))?;

    match (number.value, number.suffix) {
        (ast::NumberValue::Float(n), _) => Ok(hir::ExprKind::Lit(hir::Lit::Float(-n))),
        (ast::NumberValue::Integer(int), Some(ast::NumberSuffix::Unsigned(_, size))) => {
            let Some(n) = int.neg().to_u64() else {
                return Err(compile::Error::new(
                    ast,
                    ErrorKind::BadUnsignedOutOfBounds { size },
                ));
            };

            if !size.unsigned_in(n) {
                return Err(compile::Error::new(
                    ast,
                    ErrorKind::BadUnsignedOutOfBounds { size },
                ));
            }

            Ok(hir::ExprKind::Lit(hir::Lit::Unsigned(n)))
        }
        (ast::NumberValue::Integer(int), Some(ast::NumberSuffix::Signed(_, size))) => {
            let Some(n) = int.neg().to_i64() else {
                return Err(compile::Error::new(
                    ast,
                    ErrorKind::BadSignedOutOfBounds { size },
                ));
            };

            if !size.signed_in(n) {
                return Err(compile::Error::new(
                    ast,
                    ErrorKind::BadSignedOutOfBounds { size },
                ));
            }

            Ok(hir::ExprKind::Lit(hir::Lit::Signed(n)))
        }
        (ast::NumberValue::Integer(int), _) => {
            let Some(n) = int.neg().to_i64() else {
                return Err(compile::Error::new(
                    ast,
                    ErrorKind::BadSignedOutOfBounds {
                        size: NumberSize::S64,
                    },
                ));
            };

            Ok(hir::ExprKind::Lit(hir::Lit::Signed(n)))
        }
    }
}

/// Lower a block expression.
#[instrument_ast(span = ast)]
fn expr_block<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::ExprBlock,
) -> compile::Result<hir::ExprKind<'hir>> {
    /// The kind of an [ExprBlock].
    #[derive(Debug, Clone, Copy, PartialEq)]
    #[non_exhaustive]
    pub(crate) enum ExprBlockKind {
        Default,
        Async,
        Const,
    }

    alloc_with!(cx, ast);

    let kind = match (&ast.async_token, &ast.const_token) {
        (Some(..), None) => ExprBlockKind::Async,
        (None, Some(..)) => ExprBlockKind::Const,
        _ => ExprBlockKind::Default,
    };

    if let ExprBlockKind::Default = kind {
        return Ok(hir::ExprKind::Block(alloc!(block(
            cx,
            ast.label.as_ref(),
            &ast.block
        )?)));
    }

    if cx.const_eval {
        // This only happens if the ast expression has not been indexed. Which
        // only occurs during certain kinds of constant evaluation. So we limit
        // evaluation to only support constant blocks.
        let ExprBlockKind::Const = kind else {
            return Err(compile::Error::msg(
                ast,
                "Only constant blocks are supported in this context",
            ));
        };

        if let Some(label) = &ast.label {
            return Err(compile::Error::msg(
                label,
                "Constant blocks cannot be labelled",
            ));
        };

        return Ok(hir::ExprKind::Block(alloc!(block(cx, None, &ast.block)?)));
    };

    let item =
        cx.q.item_for("lowering block", ast.block.id)
            .with_span(&ast.block)?;
    let meta = cx.lookup_meta(ast, item.item, GenericsParameters::default())?;

    match (kind, &meta.kind) {
        (ExprBlockKind::Async, &meta::Kind::AsyncBlock { call, do_move, .. }) => {
            tracing::trace!("queuing async block build entry");

            if let Some(label) = &ast.label {
                return Err(compile::Error::msg(
                    label,
                    "Async blocks cannot be labelled",
                ));
            };

            cx.scopes.push_captures()?;
            let block = alloc!(block(cx, None, &ast.block)?);
            let layer = cx.scopes.pop().with_span(&ast.block)?;

            cx.q.set_used(&meta.item_meta)?;

            let captures = &*iter!(layer.captures().map(|(_, id)| id));

            let Some(queue) = cx.secondary_builds.as_mut() else {
                return Err(compile::Error::new(ast, ErrorKind::AsyncBlockInConst));
            };

            queue.try_push(SecondaryBuildEntry {
                item_meta: meta.item_meta,
                build: SecondaryBuild::AsyncBlock(AsyncBlock {
                    hir: alloc!(hir::AsyncBlock { block, captures }),
                    call,
                }),
            })?;

            Ok(hir::ExprKind::AsyncBlock(alloc!(hir::ExprAsyncBlock {
                hash: meta.hash,
                do_move,
                captures,
            })))
        }
        (ExprBlockKind::Const, meta::Kind::Const) => Ok(hir::ExprKind::Const(meta.hash)),
        _ => Err(compile::Error::expected_meta(
            ast,
            meta.info(cx.q.pool)?,
            "async or const block",
        )),
    }
}

/// Unroll a break expression, capturing all variables which are in scope at
/// the time of it.
fn expr_break<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::ExprBreak,
) -> compile::Result<hir::ExprBreak<'hir>> {
    alloc_with!(cx, ast);

    let label = match &ast.label {
        Some(label) => Some(label.resolve(resolve_context!(cx.q))?),
        None => None,
    };

    let Some(drop) = cx.scopes.loop_drop(label)? else {
        if let Some(label) = label {
            return Err(compile::Error::new(
                ast,
                ErrorKind::MissingLabel {
                    label: label.try_into()?,
                },
            ));
        } else {
            return Err(compile::Error::new(ast, ErrorKind::BreakUnsupported));
        }
    };

    Ok(hir::ExprBreak {
        label: match label {
            Some(label) => Some(alloc_str!(label)),
            None => None,
        },
        expr: match &ast.expr {
            Some(ast) => Some(alloc!(expr(cx, ast)?)),
            None => None,
        },
        drop: iter!(drop),
    })
}

/// Unroll a continue expression, capturing all variables which are in scope at
/// the time of it.
fn expr_continue<'hir>(
    cx: &Ctxt<'hir, '_, '_>,
    ast: &ast::ExprContinue,
) -> compile::Result<hir::ExprContinue<'hir>> {
    alloc_with!(cx, ast);

    let label = match &ast.label {
        Some(label) => Some(label.resolve(resolve_context!(cx.q))?),
        None => None,
    };

    let Some(drop) = cx.scopes.loop_drop(label)? else {
        if let Some(label) = label {
            return Err(compile::Error::new(
                ast,
                ErrorKind::MissingLabel {
                    label: label.try_into()?,
                },
            ));
        } else {
            return Err(compile::Error::new(ast, ErrorKind::ContinueUnsupported));
        }
    };

    Ok(hir::ExprContinue {
        label: match label {
            Some(label) => Some(alloc_str!(label)),
            None => None,
        },
        drop: iter!(drop),
    })
}

/// Lower a function argument.
fn fn_arg<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::FnArg,
) -> compile::Result<hir::FnArg<'hir>> {
    alloc_with!(cx, ast);

    Ok(match ast {
        ast::FnArg::SelfValue(ast) => {
            let id = cx.scopes.define(hir::Name::SelfValue, ast)?;
            hir::FnArg::SelfValue(ast.span(), id)
        }
        ast::FnArg::Pat(ast) => hir::FnArg::Pat(alloc!(pat_binding(cx, ast)?)),
    })
}

/// Lower an assignment.
fn local<'hir>(cx: &mut Ctxt<'hir, '_, '_>, ast: &ast::Local) -> compile::Result<hir::Local<'hir>> {
    // Note: expression needs to be assembled before pattern, otherwise the
    // expression will see declarations in the pattern.
    let expr = expr(cx, &ast.expr)?;
    let pat = pat_binding(cx, &ast.pat)?;

    Ok(hir::Local {
        span: ast.span(),
        pat,
        expr,
    })
}

/// The is a simple locals optimization which unpacks locals from a tuple and
/// assigns them directly to local.
fn unpack_locals(cx: &mut Ctxt<'_, '_, '_>, p: &ast::Pat, e: &ast::Expr) -> compile::Result<bool> {
    alloc_with!(cx, p);

    match (p, e) {
        (p @ ast::Pat::Path(inner), e) => {
            let Some(ast::PathKind::Ident(..)) = inner.path.as_kind() else {
                return Ok(false);
            };

            let e = expr(cx, e)?;
            let p = pat_binding(cx, p)?;

            cx.statement_buffer
                .try_push(hir::Stmt::Local(alloc!(hir::Local {
                    span: p.span().join(e.span()),
                    pat: p,
                    expr: e,
                })))?;

            return Ok(true);
        }
        (ast::Pat::Tuple(p), ast::Expr::Tuple(e)) => {
            if p.items.len() != e.items.len() {
                return Ok(false);
            }

            for ((_, _), (p, _)) in e.items.iter().zip(&p.items) {
                if matches!(p, ast::Pat::Rest(..)) {
                    return Ok(false);
                }
            }

            let mut exprs = Vec::new();

            for (e, _) in &e.items {
                exprs.try_push(expr(cx, e)?)?;
            }

            for (e, (p, _)) in exprs.into_iter().zip(&p.items) {
                let p = pat_binding(cx, p)?;

                cx.statement_buffer
                    .try_push(hir::Stmt::Local(alloc!(hir::Local {
                        span: p.span().join(e.span()),
                        pat: p,
                        expr: e,
                    })))?;
            }

            return Ok(true);
        }
        _ => {}
    };

    Ok(false)
}

fn pat_binding<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::Pat,
) -> compile::Result<hir::PatBinding<'hir>> {
    alloc_with!(cx, ast);

    let pat = pat(cx, ast)?;
    let names = iter!(cx.pattern_bindings.drain(..));

    Ok(hir::PatBinding { pat, names })
}

fn pat<'hir>(cx: &mut Ctxt<'hir, '_, '_>, ast: &ast::Pat) -> compile::Result<hir::Pat<'hir>> {
    fn filter((ast, _): &(ast::Pat, Option<ast::Comma>)) -> Option<&ast::Pat> {
        if matches!(ast, ast::Pat::Binding(..) | ast::Pat::Rest(..)) {
            return None;
        }

        Some(ast)
    }

    alloc_with!(cx, ast);

    let kind = {
        match ast {
            ast::Pat::Ignore(..) => hir::PatKind::Ignore,
            ast::Pat::Path(ast) => {
                let named = cx.q.convert_path(&ast.path)?;
                let parameters = generics_parameters(cx, &named)?;

                let path = 'path: {
                    if let Some(meta) = cx.try_lookup_meta(&ast, named.item, &parameters)? {
                        match meta.kind {
                            meta::Kind::Const => {
                                let Some(const_value) = cx.q.get_const_value(meta.hash) else {
                                    return Err(compile::Error::msg(
                                        ast,
                                        try_format!("Missing constant for hash {}", meta.hash),
                                    ));
                                };

                                let const_value = const_value.try_clone().with_span(ast)?;
                                return pat_const_value(cx, &const_value, ast);
                            }
                            _ => {
                                if let Some((0, kind)) = tuple_match_for(&meta) {
                                    break 'path hir::PatPathKind::Kind(alloc!(kind));
                                }
                            }
                        }
                    };

                    if let Some(ident) = ast.path.try_as_ident() {
                        let name = alloc_str!(ident.resolve(resolve_context!(cx.q))?);
                        let name = cx.scopes.define(hir::Name::Str(name), ast)?;
                        cx.pattern_bindings.try_push(name)?;
                        break 'path hir::PatPathKind::Ident(name);
                    }

                    return Err(compile::Error::new(ast, ErrorKind::UnsupportedBinding));
                };

                hir::PatKind::Path(alloc!(path))
            }
            ast::Pat::Lit(ast) => hir::PatKind::Lit(alloc!(expr(cx, &ast.expr)?)),
            ast::Pat::Vec(ast) => {
                let (is_open, count) = pat_items_count(ast.items.as_slice())?;
                let items = iter!(
                    ast.items.iter().filter_map(filter),
                    ast.items.len(),
                    |ast| pat(cx, ast)?
                );

                hir::PatKind::Sequence(alloc!(hir::PatSequence {
                    kind: hir::PatSequenceKind::Sequence {
                        hash: runtime::Vec::HASH,
                        count,
                        is_open
                    },
                    items,
                }))
            }
            ast::Pat::Tuple(ast) => {
                let (is_open, count) = pat_items_count(ast.items.as_slice())?;
                let items = iter!(
                    ast.items.iter().filter_map(filter),
                    ast.items.len(),
                    |ast| pat(cx, ast)?
                );

                let kind = if let Some(path) = &ast.path {
                    let named = cx.q.convert_path(path)?;
                    let parameters = generics_parameters(cx, &named)?;
                    let meta = cx.lookup_meta(path, named.item, parameters)?;

                    // Treat the current meta as a tuple and get the number of arguments it
                    // should receive and the type check that applies to it.
                    let Some((args, kind)) = tuple_match_for(&meta) else {
                        return Err(compile::Error::expected_meta(
                            path,
                            meta.info(cx.q.pool)?,
                            "type that can be used in a tuple pattern",
                        ));
                    };

                    if !(args == count || count < args && is_open) {
                        return Err(compile::Error::new(
                            path,
                            ErrorKind::BadArgumentCount {
                                expected: args,
                                actual: count,
                            },
                        ));
                    }

                    kind
                } else {
                    hir::PatSequenceKind::Sequence {
                        hash: runtime::Tuple::HASH,
                        count,
                        is_open,
                    }
                };

                hir::PatKind::Sequence(alloc!(hir::PatSequence { kind, items }))
            }
            ast::Pat::Object(ast) => {
                let (is_open, count) = pat_items_count(ast.items.as_slice())?;

                let mut keys_dup = HashMap::new();

                let bindings = iter!(ast.items.iter().take(count), |(pat, _)| {
                    let (key, binding) = match pat {
                        ast::Pat::Binding(binding) => {
                            let (span, key) = object_key(cx, &binding.key)?;
                            (
                                key,
                                hir::Binding::Binding(
                                    span.span(),
                                    key,
                                    alloc!(self::pat(cx, &binding.pat)?),
                                ),
                            )
                        }
                        ast::Pat::Path(path) => {
                            let Some(ident) = path.path.try_as_ident() else {
                                return Err(compile::Error::new(
                                    path,
                                    ErrorKind::UnsupportedPatternExpr,
                                ));
                            };

                            let key = alloc_str!(ident.resolve(resolve_context!(cx.q))?);
                            let id = cx.scopes.define(hir::Name::Str(key), ident)?;
                            cx.pattern_bindings.try_push(id)?;
                            (key, hir::Binding::Ident(path.span(), key, id))
                        }
                        _ => {
                            return Err(compile::Error::new(
                                pat,
                                ErrorKind::UnsupportedPatternExpr,
                            ));
                        }
                    };

                    if let Some(_existing) = keys_dup.try_insert(key, pat)? {
                        return Err(compile::Error::new(
                            pat,
                            ErrorKind::DuplicateObjectKey {
                                #[cfg(feature = "emit")]
                                existing: _existing.span(),
                                #[cfg(feature = "emit")]
                                object: pat.span(),
                            },
                        ));
                    }

                    binding
                });

                let kind = match &ast.ident {
                    ast::ObjectIdent::Named(path) => {
                        let named = cx.q.convert_path(path)?;
                        let parameters = generics_parameters(cx, &named)?;
                        let meta = cx.lookup_meta(path, named.item, parameters)?;

                        let Some((mut fields, kind)) =
                            struct_match_for(&meta, is_open && count == 0)?
                        else {
                            return Err(compile::Error::expected_meta(
                                path,
                                meta.info(cx.q.pool)?,
                                "type that can be used in a struct pattern",
                            ));
                        };

                        for binding in bindings.iter() {
                            if !fields.remove(binding.key()) {
                                return Err(compile::Error::new(
                                    ast,
                                    ErrorKind::LitObjectNotField {
                                        field: binding.key().try_into()?,
                                        item: cx.q.pool.item(meta.item_meta.item).try_to_owned()?,
                                    },
                                ));
                            }
                        }

                        if !is_open && !fields.is_empty() {
                            let mut fields = fields.into_iter().try_collect::<Box<[_]>>()?;

                            fields.sort();

                            return Err(compile::Error::new(
                                ast,
                                ErrorKind::PatternMissingFields {
                                    item: cx.q.pool.item(meta.item_meta.item).try_to_owned()?,
                                    #[cfg(feature = "emit")]
                                    fields,
                                },
                            ));
                        }

                        kind
                    }
                    ast::ObjectIdent::Anonymous(..) => hir::PatSequenceKind::Sequence {
                        hash: runtime::Object::HASH,
                        count,
                        is_open,
                    },
                };

                hir::PatKind::Object(alloc!(hir::PatObject { kind, bindings }))
            }
            _ => {
                return Err(compile::Error::new(ast, ErrorKind::UnsupportedPatternExpr));
            }
        }
    };

    Ok(hir::Pat {
        span: ast.span(),
        kind,
    })
}

fn object_key<'hir, 'ast>(
    cx: &Ctxt<'hir, '_, '_>,
    ast: &'ast ast::ObjectKey,
) -> compile::Result<(&'ast dyn Spanned, &'hir str)> {
    alloc_with!(cx, ast);

    Ok(match ast {
        ast::ObjectKey::LitStr(lit) => {
            let string = lit.resolve(resolve_context!(cx.q))?;
            (lit, alloc_str!(string.as_ref()))
        }
        ast::ObjectKey::Path(ast) => {
            let Some(ident) = ast.try_as_ident() else {
                return Err(compile::Error::expected(ast, "object key"));
            };

            let string = ident.resolve(resolve_context!(cx.q))?;
            (ident, alloc_str!(string))
        }
    })
}

/// Lower the given path.
#[instrument_ast(span = ast)]
fn expr_path<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::Path,
    in_path: bool,
) -> compile::Result<hir::ExprKind<'hir>> {
    alloc_with!(cx, ast);

    if let Some(ast::PathKind::SelfValue) = ast.as_kind() {
        let Some((id, _)) = cx.scopes.get(hir::Name::SelfValue)? else {
            return Err(compile::Error::new(ast, ErrorKind::MissingSelf));
        };

        return Ok(hir::ExprKind::Variable(id));
    }

    if let Needs::Value = cx.needs {
        if let Some(name) = ast.try_as_ident() {
            let name = alloc_str!(name.resolve(resolve_context!(cx.q))?);

            if let Some((name, _)) = cx.scopes.get(hir::Name::Str(name))? {
                return Ok(hir::ExprKind::Variable(name));
            }
        }
    }

    // Caller has indicated that if they can't have a variable, they do indeed
    // want a path.
    if in_path {
        return Ok(hir::ExprKind::Path);
    }

    let named = cx.q.convert_path(ast)?;
    let parameters = generics_parameters(cx, &named)?;

    if let Some(meta) = cx.try_lookup_meta(ast, named.item, &parameters)? {
        return expr_path_meta(cx, &meta, ast);
    }

    if let (Needs::Value, Some(local)) = (cx.needs, ast.try_as_ident()) {
        let local = local.resolve(resolve_context!(cx.q))?;

        // light heuristics, treat it as a type error in case the first
        // character is uppercase.
        if !local.starts_with(char::is_uppercase) {
            return Err(compile::Error::new(
                ast,
                ErrorKind::MissingLocal {
                    name: Box::<str>::try_from(local)?,
                },
            ));
        }
    }

    let kind = if !parameters.parameters.is_empty() {
        ErrorKind::MissingItemParameters {
            item: cx.q.pool.item(named.item).try_to_owned()?,
            parameters: parameters.parameters,
        }
    } else {
        ErrorKind::MissingItem {
            item: cx.q.pool.item(named.item).try_to_owned()?,
        }
    };

    Err(compile::Error::new(ast, kind))
}

/// Compile an item.
#[instrument_ast(span = span)]
fn expr_path_meta<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    meta: &meta::Meta,
    span: &dyn Spanned,
) -> compile::Result<hir::ExprKind<'hir>> {
    alloc_with!(cx, span);

    if let Needs::Value = cx.needs {
        match &meta.kind {
            meta::Kind::Struct {
                fields: meta::Fields::Empty,
                ..
            } => Ok(hir::ExprKind::Call(alloc!(hir::ExprCall {
                call: hir::Call::Meta { hash: meta.hash },
                args: &[],
            }))),
            meta::Kind::Struct {
                fields: meta::Fields::Unnamed(0),
                ..
            } => Ok(hir::ExprKind::Call(alloc!(hir::ExprCall {
                call: hir::Call::Meta { hash: meta.hash },
                args: &[],
            }))),
            meta::Kind::Struct {
                fields: meta::Fields::Unnamed(..),
                ..
            } => Ok(hir::ExprKind::Fn(meta.hash)),
            meta::Kind::Function { .. } => Ok(hir::ExprKind::Fn(meta.hash)),
            meta::Kind::Const => Ok(hir::ExprKind::Const(meta.hash)),
            meta::Kind::Struct { .. } | meta::Kind::Type { .. } | meta::Kind::Enum { .. } => {
                Ok(hir::ExprKind::Type(Type::new(meta.hash)))
            }
            _ => Err(compile::Error::expected_meta(
                span,
                meta.info(cx.q.pool)?,
                "something that can be used as a value",
            )),
        }
    } else {
        let Some(type_hash) = meta.type_hash_of() else {
            return Err(compile::Error::expected_meta(
                span,
                meta.info(cx.q.pool)?,
                "something that has a type",
            ));
        };

        Ok(hir::ExprKind::Type(Type::new(type_hash)))
    }
}

fn condition<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::Condition,
) -> compile::Result<hir::Condition<'hir>> {
    alloc_with!(cx, ast);

    Ok(match ast {
        ast::Condition::Expr(ast) => hir::Condition::Expr(alloc!(expr(cx, ast)?)),
        ast::Condition::ExprLet(ast) => hir::Condition::ExprLet(alloc!(hir::ExprLet {
            pat: pat_binding(cx, &ast.pat)?,
            expr: expr(cx, &ast.expr)?,
        })),
    })
}

/// Test if the given pattern is open or not.
fn pat_items_count(items: &[(ast::Pat, Option<ast::Comma>)]) -> compile::Result<(bool, usize)> {
    let mut it = items.iter();

    let (is_open, mut count) = match it.next_back() {
        Some((pat, _)) => matches!(pat, ast::Pat::Rest { .. })
            .then(|| (true, 0))
            .unwrap_or((false, 1)),
        None => return Ok((false, 0)),
    };

    for (pat, _) in it {
        if let ast::Pat::Rest { .. } = pat {
            return Err(compile::Error::new(pat, ErrorKind::UnsupportedPatternRest));
        }

        count += 1;
    }

    Ok((is_open, count))
}

/// Generate a legal struct match for the given meta which indicates the type of
/// sequence and the fields that it expects.
///
/// For `open` matches (i.e. `{ .. }`), `Unnamed` and `Empty` structs are also
/// supported and they report empty fields.
fn struct_match_for(
    meta: &meta::Meta,
    open: bool,
) -> alloc::Result<Option<(HashSet<Box<str>>, hir::PatSequenceKind)>> {
    let (fields, kind) = match meta.kind {
        meta::Kind::Struct {
            ref fields,
            enum_hash,
            ..
        } => {
            let kind = 'kind: {
                if enum_hash != Hash::EMPTY {
                    break 'kind hir::PatSequenceKind::Type {
                        hash: enum_hash,
                        variant_hash: meta.hash,
                    };
                }

                hir::PatSequenceKind::Type {
                    hash: meta.hash,
                    variant_hash: Hash::EMPTY,
                }
            };

            (fields, kind)
        }
        meta::Kind::Type { .. } if open => {
            return Ok(Some((
                HashSet::new(),
                hir::PatSequenceKind::Type {
                    hash: meta.hash,
                    variant_hash: Hash::EMPTY,
                },
            )));
        }
        _ => {
            return Ok(None);
        }
    };

    let fields = match fields {
        meta::Fields::Named(st) => st
            .fields
            .iter()
            .map(|f| f.name.try_clone())
            .try_collect::<alloc::Result<_>>()??,
        _ if open => HashSet::new(),
        _ => return Ok(None),
    };

    Ok(Some((fields, kind)))
}

fn tuple_match_for(meta: &meta::Meta) -> Option<(usize, hir::PatSequenceKind)> {
    match meta.kind {
        meta::Kind::Struct {
            ref fields,
            enum_hash,
            ..
        } => {
            let args = match *fields {
                meta::Fields::Unnamed(args) => args,
                meta::Fields::Empty => 0,
                _ => return None,
            };

            let kind = 'kind: {
                if enum_hash != Hash::EMPTY {
                    break 'kind hir::PatSequenceKind::Type {
                        hash: enum_hash,
                        variant_hash: meta.hash,
                    };
                }

                hir::PatSequenceKind::Type {
                    hash: meta.hash,
                    variant_hash: Hash::EMPTY,
                }
            };

            Some((args, kind))
        }
        _ => None,
    }
}

fn generics_parameters(
    cx: &mut Ctxt<'_, '_, '_>,
    named: &Named<'_>,
) -> compile::Result<GenericsParameters> {
    let mut parameters = GenericsParameters {
        trailing: named.trailing,
        parameters: [None, None],
    };

    for (value, o) in named
        .parameters
        .iter()
        .zip(parameters.parameters.iter_mut())
    {
        if let &Some((span, generics)) = value {
            let mut builder = ParametersBuilder::new();

            for (s, _) in generics {
                let hir::ExprKind::Type(ty) = expr(cx, &s.expr)?.kind else {
                    return Err(compile::Error::new(s, ErrorKind::UnsupportedGenerics));
                };

                builder = builder.add(ty.into_hash()).with_span(span)?;
            }

            *o = Some(builder.finish());
        }
    }

    Ok(parameters)
}

/// Convert into a call expression.
#[instrument_ast(span = ast)]
fn expr_call<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::ExprCall,
) -> compile::Result<hir::ExprCall<'hir>> {
    fn find_path(ast: &ast::Expr) -> Option<&ast::Path> {
        let mut current = ast;

        loop {
            match current {
                ast::Expr::Path(path) => return Some(path),
                ast::Expr::Empty(ast) => {
                    current = &*ast.expr;
                    continue;
                }
                _ => return None,
            }
        }
    }

    alloc_with!(cx, ast);

    let in_path = replace(&mut cx.in_path, true);
    let expr = expr(cx, &ast.expr)?;
    cx.in_path = in_path;

    let call = 'ok: {
        match expr.kind {
            hir::ExprKind::Variable(name) => {
                break 'ok hir::Call::Var { name };
            }
            hir::ExprKind::Path => {
                let Some(path) = find_path(&ast.expr) else {
                    return Err(compile::Error::msg(&ast.expr, "Expected path"));
                };

                let named = cx.q.convert_path(path)?;
                let parameters = generics_parameters(cx, &named)?;

                let meta = cx.lookup_meta(path, named.item, parameters)?;
                debug_assert_eq!(meta.item_meta.item, named.item);

                match &meta.kind {
                    meta::Kind::Struct {
                        fields: meta::Fields::Empty,
                        ..
                    } => {
                        if !ast.args.is_empty() {
                            return Err(compile::Error::new(
                                &ast.args,
                                ErrorKind::BadArgumentCount {
                                    expected: 0,
                                    actual: ast.args.len(),
                                },
                            ));
                        }
                    }
                    meta::Kind::Struct {
                        fields: meta::Fields::Unnamed(args),
                        ..
                    } => {
                        if *args != ast.args.len() {
                            return Err(compile::Error::new(
                                &ast.args,
                                ErrorKind::BadArgumentCount {
                                    expected: *args,
                                    actual: ast.args.len(),
                                },
                            ));
                        }

                        if *args == 0 {
                            cx.q.diagnostics.remove_tuple_call_parens(
                                cx.source_id,
                                &ast.args,
                                path,
                                None,
                            )?;
                        }
                    }
                    meta::Kind::Function { .. } => {
                        if let Some(message) = cx.q.lookup_deprecation(meta.hash) {
                            cx.q.diagnostics.used_deprecated(
                                cx.source_id,
                                &expr.span,
                                None,
                                message.try_into()?,
                            )?;
                        };
                    }
                    meta::Kind::ConstFn => {
                        let from =
                            cx.q.item_for("lowering constant function", ast.id)
                                .with_span(ast)?;

                        break 'ok hir::Call::ConstFn {
                            from_module: from.module,
                            from_item: from.item,
                            id: meta.item_meta.item,
                        };
                    }
                    _ => {
                        return Err(compile::Error::expected_meta(
                            ast,
                            meta.info(cx.q.pool)?,
                            "something that can be called as a function",
                        ));
                    }
                };

                break 'ok hir::Call::Meta { hash: meta.hash };
            }
            hir::ExprKind::FieldAccess(&hir::ExprFieldAccess {
                expr_field,
                expr: target,
            }) => {
                let hash = match expr_field {
                    hir::ExprField::Index(index) => Hash::index(index),
                    hir::ExprField::Ident(ident) => {
                        cx.q.unit.insert_debug_ident(ident)?;
                        Hash::ident(ident)
                    }
                    hir::ExprField::IdentGenerics(ident, hash) => {
                        cx.q.unit.insert_debug_ident(ident)?;
                        Hash::ident(ident).with_function_parameters(hash)
                    }
                };

                break 'ok hir::Call::Associated {
                    target: alloc!(target),
                    hash,
                };
            }
            _ => {}
        }

        break 'ok hir::Call::Expr { expr: alloc!(expr) };
    };

    Ok(hir::ExprCall {
        call,
        args: iter!(&ast.args, |(ast, _)| self::expr(cx, ast)?),
    })
}

#[instrument_ast(span = ast)]
fn expr_field_access<'hir>(
    cx: &mut Ctxt<'hir, '_, '_>,
    ast: &ast::ExprFieldAccess,
) -> compile::Result<hir::ExprFieldAccess<'hir>> {
    alloc_with!(cx, ast);

    let expr_field = match &ast.expr_field {
        ast::ExprField::LitNumber(ast) => {
            let number = ast.resolve(resolve_context!(cx.q))?;

            let Some(index) = number.as_tuple_index() else {
                return Err(compile::Error::new(
                    ast,
                    ErrorKind::UnsupportedTupleIndex { number },
                ));
            };

            hir::ExprField::Index(index)
        }
        ast::ExprField::Path(ast) => {
            let Some((ident, generics)) = ast.try_as_ident_generics() else {
                return Err(compile::Error::new(ast, ErrorKind::BadFieldAccess));
            };

            let ident = alloc_str!(ident.resolve(resolve_context!(cx.q))?);

            match generics {
                Some(generics) => {
                    let mut builder = ParametersBuilder::new();

                    for (s, _) in generics {
                        let hir::ExprKind::Type(ty) = expr(cx, &s.expr)?.kind else {
                            return Err(compile::Error::new(s, ErrorKind::UnsupportedGenerics));
                        };

                        builder = builder.add(ty.into_hash()).with_span(s)?;
                    }

                    hir::ExprField::IdentGenerics(ident, builder.finish())
                }
                None => hir::ExprField::Ident(ident),
            }
        }
    };

    Ok(hir::ExprFieldAccess {
        expr: expr(cx, &ast.expr)?,
        expr_field,
    })
}
