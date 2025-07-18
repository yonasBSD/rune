use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, Result};

use crate::cli::{AssetKind, CommandBase, Config, ExitCode, Io, SharedFlags};
use crate::runtime::{UnitStorage, VmError, VmExecution, VmOutcome};
use crate::sync::Arc;
use crate::{Context, Hash, Sources, Unit, Value, Vm};

mod cli {
    use std::path::PathBuf;
    use std::vec::Vec;

    use clap::Parser;

    #[derive(Parser, Debug)]
    #[command(rename_all = "kebab-case")]
    pub(crate) struct Flags {
        /// Provide detailed tracing for each instruction executed.
        #[arg(short, long)]
        pub(super) trace: bool,
        /// When tracing is enabled, do not include source references if they are
        /// available.
        #[arg(long)]
        pub(super) without_source: bool,
        /// Time how long the script took to execute.
        #[arg(long)]
        pub(super) time: bool,
        /// Perform a default dump.
        #[arg(short, long)]
        pub(super) dump: bool,
        /// Dump return value.
        #[arg(long)]
        pub(super) dump_return: bool,
        /// Dump everything that is available, this is very verbose.
        #[arg(long)]
        pub(super) dump_all: bool,
        /// Dump default information about unit.
        #[arg(long)]
        pub(super) dump_unit: bool,
        /// Dump constants from the unit.
        #[arg(long)]
        pub(super) dump_constants: bool,
        /// Dump unit instructions.
        #[arg(long)]
        pub(super) emit_instructions: bool,
        /// Dump the state of the stack after completion.
        ///
        /// If compiled with `--trace` will dump it after each instruction.
        #[arg(long)]
        pub(super) dump_stack: bool,
        /// Dump dynamic functions.
        #[arg(long)]
        pub(super) dump_functions: bool,
        /// Dump dynamic types.
        #[arg(long)]
        pub(super) dump_types: bool,
        /// Dump native functions.
        #[arg(long)]
        pub(super) dump_native_functions: bool,
        /// Dump native types.
        #[arg(long)]
        pub(super) dump_native_types: bool,
        /// When tracing, limit the number of instructions to run with `limit`. This
        /// implies `--trace`.
        #[arg(long)]
        pub(super) trace_limit: Option<usize>,
        /// Explicit paths to run.
        pub(super) run_path: Vec<PathBuf>,
    }
}

pub(super) use cli::Flags;

impl CommandBase for Flags {
    #[inline]
    fn is_workspace(&self, kind: AssetKind) -> bool {
        matches!(kind, AssetKind::Bin)
    }

    #[inline]
    fn propagate(&mut self, _: &mut Config, _: &mut SharedFlags) {
        if self.dump || self.dump_all {
            self.dump_unit = true;
            self.dump_stack = true;
            self.dump_return = true;
        }

        if self.dump_all {
            self.dump_constants = true;
            self.dump_functions = true;
            self.dump_types = true;
            self.dump_native_functions = true;
            self.dump_native_types = true;
        }

        if self.dump_functions
            || self.dump_native_functions
            || self.dump_stack
            || self.dump_types
            || self.dump_constants
        {
            self.dump_unit = true;
        }

        if self.dump_unit {
            self.emit_instructions = true;
        }

        if self.trace_limit.is_some() {
            self.trace = true;
        }

        if self.trace {
            self.dump_return = true;
        }
    }

    fn paths(&self) -> &[PathBuf] {
        &self.run_path
    }
}

enum TraceError {
    Io(std::io::Error),
    VmError(VmError),
    Limited,
}

impl From<std::io::Error> for TraceError {
    #[inline]
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<VmError> for TraceError {
    #[inline]
    fn from(error: VmError) -> Self {
        Self::VmError(error)
    }
}

pub(super) async fn run(
    io: &mut Io<'_>,
    c: &Config,
    args: &Flags,
    context: &Context,
    unit: Arc<Unit>,
    sources: &Sources,
    entry: Hash,
) -> Result<ExitCode> {
    if args.dump_native_functions {
        writeln!(io.stdout, "# functions")?;

        for (i, (meta, _)) in context.iter_functions().enumerate() {
            if let Some(item) = &meta.item {
                writeln!(io.stdout, "{:04} = {} ({})", i, item, meta.hash)?;
            }
        }
    }

    if args.dump_native_types {
        writeln!(io.stdout, "# types")?;

        for (i, (hash, ty)) in context.iter_types().enumerate() {
            writeln!(io.stdout, "{i:04} = {ty} ({hash})")?;
        }
    }

    if args.dump_unit {
        writeln!(
            io.stdout,
            "Unit size: {} bytes",
            unit.instructions().bytes()
        )?;

        if args.emit_instructions {
            let mut o = io.stdout.lock();
            writeln!(o, "# instructions")?;
            unit.emit_instructions(&mut o, sources, args.without_source)?;
        }

        let mut functions = unit.iter_functions().peekable();
        let mut strings = unit.iter_static_strings().peekable();
        let mut bytes = unit.iter_static_bytes().peekable();
        let mut drop_sets = unit.iter_static_drop_sets().peekable();
        let mut keys = unit.iter_static_object_keys().peekable();
        let mut constants = unit.iter_constants().peekable();

        if args.dump_functions && functions.peek().is_some() {
            writeln!(io.stdout, "# dynamic functions")?;

            for (hash, kind) in functions {
                if let Some(signature) = unit.debug_info().and_then(|d| d.functions.get(&hash)) {
                    writeln!(io.stdout, "{hash} = {signature}")?;
                } else {
                    writeln!(io.stdout, "{hash} = {kind}")?;
                }
            }
        }

        if strings.peek().is_some() {
            writeln!(io.stdout, "# strings")?;

            for (i, string) in strings.enumerate() {
                writeln!(io.stdout, "{i} = {string:?}")?;
            }
        }

        if bytes.peek().is_some() {
            writeln!(io.stdout, "# bytes")?;

            for (i, bytes) in bytes.enumerate() {
                writeln!(io.stdout, "{i} = {bytes:?}")?;
            }
        }

        if keys.peek().is_some() {
            writeln!(io.stdout, "# object keys")?;

            for (hash, keys) in keys {
                writeln!(io.stdout, "{hash} = {keys:?}")?;
            }
        }

        if drop_sets.peek().is_some() {
            writeln!(io.stdout, "# drop sets")?;

            for (i, set) in drop_sets.enumerate() {
                writeln!(io.stdout, "{i} = {set:?}")?;
            }
        }

        if args.dump_constants && constants.peek().is_some() {
            writeln!(io.stdout, "# constants")?;

            for (hash, constant) in constants {
                writeln!(io.stdout, "{hash} = {constant:?}")?;
            }
        }
    }

    let runtime = Arc::try_new(context.runtime()?)?;

    let last = Instant::now();

    let mut vm = Vm::new(runtime, unit);
    let mut execution: VmExecution<_> = vm.execute(entry, ())?;

    let result = if args.trace {
        match do_trace(
            io,
            &mut execution,
            sources,
            args.dump_stack,
            args.without_source,
            args.trace_limit.unwrap_or(usize::MAX),
        )
        .await
        {
            Ok(value) => Ok(value),
            Err(TraceError::Io(io)) => return Err(io.into()),
            Err(TraceError::VmError(vm)) => Err(vm),
            Err(TraceError::Limited) => return Err(anyhow!("Trace limit reached")),
        }
    } else {
        execution.resume().await.and_then(VmOutcome::into_complete)
    };

    let errored = match result {
        Ok(result) => {
            if c.verbose || args.time || args.dump_return {
                let duration = Instant::now().saturating_duration_since(last);

                execution
                    .vm()
                    .with(|| writeln!(io.stderr, "== {result:?} ({duration:?})"))?;
            }

            None
        }
        Err(error) => {
            if c.verbose || args.time || args.dump_return {
                let duration = Instant::now().saturating_duration_since(last);

                execution
                    .vm()
                    .with(|| writeln!(io.stderr, "== ! ({error}) ({duration:?})"))?;
            }

            Some(error)
        }
    };

    let exit = if let Some(error) = errored {
        error.emit(io.stdout, sources)?;
        ExitCode::VmError
    } else {
        ExitCode::Success
    };

    if args.dump_stack {
        writeln!(io.stdout, "# call frames after halting")?;

        let vm = execution.vm();

        let frames = vm.call_frames();
        let stack = vm.stack();

        let mut it = frames.iter().enumerate().peekable();

        while let Some((count, frame)) = it.next() {
            let stack_top = match it.peek() {
                Some((_, next)) => next.top,
                None => stack.top(),
            };

            let values = stack.get(frame.top..stack_top).expect("bad stack slice");

            writeln!(io.stdout, "  frame #{count} (+{})", frame.top)?;

            if values.is_empty() {
                writeln!(io.stdout, "    *empty*")?;
            }

            vm.with(|| {
                for (n, value) in values.iter().enumerate() {
                    writeln!(io.stdout, "    {}+{n} = {value:?}", frame.top)?;
                }

                Ok::<_, crate::support::Error>(())
            })?;
        }

        // NB: print final frame
        writeln!(io.stdout, "  frame #{} (+{})", frames.len(), stack.top())?;

        let values = stack.get(stack.top()..).expect("bad stack slice");

        if values.is_empty() {
            writeln!(io.stdout, "    *empty*")?;
        }

        vm.with(|| {
            for (n, value) in values.iter().enumerate() {
                writeln!(io.stdout, "    {}+{n} = {value:?}", stack.top())?;
            }

            Ok::<_, crate::support::Error>(())
        })?;
    }

    Ok(exit)
}

/// Perform a detailed trace of the program.
async fn do_trace<T>(
    io: &Io<'_>,
    execution: &mut VmExecution<T>,
    sources: &Sources,
    dump_stack: bool,
    without_source: bool,
    mut limit: usize,
) -> Result<Value, TraceError>
where
    T: AsRef<Vm> + AsMut<Vm>,
{
    let mut current_frame_len = execution.vm().call_frames().len();
    let mut result = None;
    let mut yielded = None;

    while limit > 0 {
        let vm = execution.vm();
        let ip = vm.ip();
        let mut o = io.stdout.lock();

        if let Some(value) = yielded.take() {
            vm.with(|| writeln!(o, "yield: {value:?}"))?;
        }

        if let Some((hash, signature)) = vm.unit().debug_info().and_then(|d| d.function_at(ip)) {
            writeln!(o, "fn {signature} ({hash}):")?;
        }

        let debug = vm.unit().debug_info().and_then(|d| d.instruction_at(ip));

        for label in debug.map(|d| d.labels.as_slice()).unwrap_or_default() {
            writeln!(o, "{label}:")?;
        }

        if dump_stack {
            let frames = vm.call_frames();
            let stack = vm.stack();

            if current_frame_len != frames.len() {
                let op = if current_frame_len < frames.len() {
                    "push"
                } else {
                    "pop"
                };
                write!(o, "  {op} frame {} (+{})", frames.len(), stack.top())?;

                if let Some(frame) = frames.last() {
                    writeln!(o, " {frame:?}")?;
                } else {
                    writeln!(o, " *root*")?;
                }

                current_frame_len = frames.len();
            }
        }

        if let Some((inst, _)) = vm.unit().instruction_at(ip).map_err(VmError::from)? {
            write!(o, "  {ip:04} = {inst}")?;
        } else {
            write!(o, "  {ip:04} = *out of bounds*")?;
        }

        if let Some(comment) = debug.and_then(|d| d.comment.as_ref()) {
            write!(o, " // {comment}")?;
        }

        writeln!(o)?;

        if !without_source {
            let debug_info = debug.and_then(|d| sources.get(d.source_id).map(|s| (s, d.span)));

            if let Some(line) = debug_info.and_then(|(s, span)| s.source_line(span)) {
                write!(o, "  ")?;
                line.write(&mut o)?;
                writeln!(o)?;
            }
        }

        if dump_stack {
            let stack = vm.stack();
            let values = stack.get(stack.top()..).expect("bad stack slice");

            if !values.is_empty() {
                vm.with(|| {
                    for (n, value) in values.iter().enumerate() {
                        writeln!(o, "    {}+{n} = {value:?}", stack.top())?;
                    }

                    Ok::<_, TraceError>(())
                })?;
            }
        }

        if let Some(value) = result {
            return Ok(value);
        }

        match execution.resume().with_budget(1).await {
            Ok(VmOutcome::Complete(value)) => {
                result = Some(value);
            }
            Ok(VmOutcome::Yielded(value)) => {
                yielded = Some(value);
            }
            Ok(VmOutcome::Limited) => {}
            Err(error) => {
                return Err(TraceError::VmError(error));
            }
        }

        limit = limit.wrapping_sub(1);
    }

    Err(TraceError::Limited)
}
