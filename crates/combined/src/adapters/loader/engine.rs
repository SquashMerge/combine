//! The engine thread: one warm QuickJS runtime with Pele evaluated in it, the caps that bound
//! a check, and the loop that serves requests. The only file that names the JS engine.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Instant;

use rquickjs::context::EvalOptions;
use rquickjs::convert::Coerced;
use rquickjs::{Context, Ctx, Error, Exception, FromJs, Function, Object, Runtime, Value};
use tokio::sync::oneshot;

use super::diagram_check::{DiagramLimits, EngineStats};
use super::pele::{
    self, Ending, Thrown, ENTRY_POINT, PELE_BUNDLE, PELE_MAX_EDGES, PELE_OUTPUT_LIMIT,
};
use crate::ports::{DiagramCheckError, DiagramVerdict};

/// How deep JS may recurse before QuickJS throws a `RangeError`.
const JS_STACK_LIMIT: usize = 1024 * 1024;
/// A heap this close to its cap after a call counts as having hit it.
const OOM_MARGIN: usize = 1024 * 1024;

/// What the engine thread is asked to do.
pub(super) enum Request {
    Check {
        source: String,
        reply: oneshot::Sender<Result<DiagramVerdict, DiagramCheckError>>,
    },
    Stats {
        reply: oneshot::Sender<Result<EngineStats, DiagramCheckError>>,
    },
}

struct Engine {
    rt: Runtime,
    ctx: Context,
    /// Nanoseconds after `origin` at which the running call is stopped; 0 means no deadline.
    deadline: Arc<AtomicU64>,
    origin: Instant,
    interrupted: Arc<AtomicBool>,
}

struct EngineState {
    limits: DiagramLimits,
    engine: Option<Engine>,
    last_build_error: Option<String>,
    heap_after_last_check: Option<u64>,
    builds: u64,
}

/// Runs on the engine thread: builds the warm runtime, reports it on `warmed`, then serves
/// `requests` in order until the channel closes.
pub(super) fn serve(
    limits: DiagramLimits,
    requests: Receiver<Request>,
    warmed: oneshot::Sender<Result<(), DiagramCheckError>>,
) {
    let mut state = EngineState::new(limits);
    state.rebuild_if_needed();
    let _ = warmed.send(match &state.engine {
        Some(_) => Ok(()),
        None => Err(state.unavailable()),
    });
    while let Ok(request) = requests.recv() {
        match request {
            Request::Check { source, reply } => {
                let _ = reply.send(state.check(&source));
                state.rebuild_if_needed();
            }
            Request::Stats { reply } => {
                let _ = reply.send(state.stats());
            }
        }
    }
}

fn describe(ctx: &Ctx<'_>, error: Error) -> String {
    match error {
        Error::Exception => {
            let thrown = thrown_from(ctx.catch());
            if thrown.thrown_null {
                "out of memory".to_owned()
            } else {
                thrown
                    .message
                    .or(thrown.name)
                    .unwrap_or_else(|| "(no message)".to_owned())
            }
        }
        other => other.to_string(),
    }
}

fn build(limits: &DiagramLimits) -> Result<Engine, String> {
    let create = |error: Error| format!("cannot create the engine: {error}");
    let rt = Runtime::new().map_err(create)?;
    rt.set_memory_limit(limits.memory_limit);
    rt.set_max_stack_size(JS_STACK_LIMIT);
    let deadline = Arc::new(AtomicU64::new(0));
    let interrupted = Arc::new(AtomicBool::new(false));
    let origin = Instant::now();
    rt.set_interrupt_handler(Some(Box::new({
        let deadline = Arc::clone(&deadline);
        let interrupted = Arc::clone(&interrupted);
        move || {
            let due = deadline.load(Ordering::Relaxed);
            let elapsed = u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
            let hit = due != 0 && elapsed >= due;
            if hit {
                interrupted.store(true, Ordering::Relaxed);
            }
            hit
        }
    })));
    let ctx = Context::full(&rt).map_err(create)?;
    ctx.with(|ctx| -> Result<(), String> {
        let mut options = EvalOptions::default();
        options.global = true;
        options.strict = true;
        ctx.eval_with_options::<(), _>(PELE_BUNDLE, options)
            .map_err(|error| format!("cannot load Pele: {}", describe(&ctx, error)))?;
        let pele: Object = ctx
            .globals()
            .get("pele")
            .map_err(|error| format!("cannot load Pele: {}", describe(&ctx, error)))?;
        for name in ["render", "parse"] {
            let is_function = pele
                .get::<_, Value>(name)
                .map(|value| value.is_function())
                .unwrap_or(false);
            if !is_function {
                return Err(format!("cannot load Pele: pele.{name} is not a function"));
            }
        }
        Ok(())
    })?;
    rt.run_gc();
    Ok(Engine {
        rt,
        ctx,
        deadline,
        origin,
        interrupted,
    })
}

fn read<'js, T: FromJs<'js>>(ctx: &Ctx<'js>, object: &Object<'js>, key: &str) -> Option<T> {
    match object.get::<_, Option<T>>(key) {
        Ok(value) => value,
        Err(Error::Exception) => {
            let _ = ctx.catch();
            None
        }
        Err(_) => None,
    }
}

/// Reads what was thrown. An Error object, a `PeleError` included, gives its bare `.message` and
/// its `name`; any other object gives its `message` property; anything else is coerced to a
/// string. `code`, `line`, `column` and `snippet` are property reads, `None` when missing or of
/// another type.
fn thrown_from<'js>(value: Value<'js>) -> Thrown {
    let ctx = value.ctx().clone();
    let mut thrown = Thrown {
        thrown_null: value.is_null(),
        ..Thrown::default()
    };
    match value.as_object() {
        Some(object) => {
            thrown.message = match Exception::from_object(object.clone()) {
                Some(error) => error.message(),
                None => read::<Coerced<String>>(&ctx, object, "message").map(|text| text.0),
            };
            thrown.name = read(&ctx, object, "name");
            thrown.code = read(&ctx, object, "code");
            thrown.line = read(&ctx, object, "line");
            thrown.column = read(&ctx, object, "column");
            thrown.snippet = read(&ctx, object, "snippet");
        }
        None => {
            thrown.message = value.get::<Coerced<String>>().ok().map(|text| text.0);
        }
    }
    thrown
}

fn call_pele<'js>(ctx: &Ctx<'js>, source: &str, limits: &DiagramLimits) -> rquickjs::Result<()> {
    let pele: Object = ctx.globals().get("pele")?;
    let entry: Function = pele.get(ENTRY_POINT)?;
    let options = Object::new(ctx.clone())?;
    options.set(
        "limit",
        u32::try_from(limits.max_source_bytes).unwrap_or(u32::MAX),
    )?;
    options.set("maxEdges", PELE_MAX_EDGES)?;
    options.set("outputLimit", PELE_OUTPUT_LIMIT)?;
    let _: Value = entry.call((source, options))?;
    Ok(())
}

impl EngineState {
    fn new(limits: DiagramLimits) -> EngineState {
        EngineState {
            limits,
            engine: None,
            last_build_error: None,
            heap_after_last_check: None,
            builds: 0,
        }
    }

    fn rebuild_if_needed(&mut self) {
        if self.engine.is_some() {
            return;
        }
        match build(&self.limits) {
            Ok(engine) => {
                self.engine = Some(engine);
                self.last_build_error = None;
                self.builds += 1;
            }
            Err(error) => self.last_build_error = Some(error),
        }
    }

    fn unavailable(&self) -> DiagramCheckError {
        DiagramCheckError::Unavailable(
            self.last_build_error
                .clone()
                .unwrap_or_else(|| "the engine is not running".to_owned()),
        )
    }

    fn check(&mut self, source: &str) -> Result<DiagramVerdict, DiagramCheckError> {
        let limits = self.limits;
        self.run_capped(source, |ctx| call_pele(ctx, source, &limits))
    }

    /// Runs `body` under the time and memory caps and classifies how it ended. A runtime that
    /// ended over a cap, or in anything but a verdict, is dropped; the next check builds anew.
    fn run_capped(
        &mut self,
        source: &str,
        body: impl FnOnce(&Ctx<'_>) -> rquickjs::Result<()>,
    ) -> Result<DiagramVerdict, DiagramCheckError> {
        self.rebuild_if_needed();
        let Some(engine) = &self.engine else {
            return Err(self.unavailable());
        };
        engine.interrupted.store(false, Ordering::Relaxed);
        let allowed = u64::try_from(self.limits.time_limit.as_nanos()).unwrap_or(u64::MAX);
        let due = u64::try_from(engine.origin.elapsed().as_nanos())
            .unwrap_or(u64::MAX)
            .saturating_add(allowed)
            .max(1);
        engine.deadline.store(due, Ordering::Relaxed);
        let thrown = engine.ctx.with(|ctx| match body(&ctx) {
            Ok(()) => None,
            Err(Error::Exception) => Some(thrown_from(ctx.catch())),
            Err(other) => Some(Thrown {
                message: Some(other.to_string()),
                ..Thrown::default()
            }),
        });
        engine.deadline.store(0, Ordering::Relaxed);
        let malloc = usize::try_from(engine.rt.memory_usage().malloc_size).unwrap_or(0);
        let thrown = thrown.map(|thrown| Thrown {
            heap_near_limit: malloc + OOM_MARGIN >= self.limits.memory_limit,
            ..thrown
        });
        self.heap_after_last_check = Some(malloc as u64);
        engine.rt.run_gc();
        let interrupted = engine.interrupted.load(Ordering::Relaxed);
        match pele::classify(thrown.as_ref(), interrupted, &self.limits, source) {
            Ending::Keep(verdict) => Ok(verdict),
            Ending::Discard(answer) => {
                self.engine = None;
                answer
            }
        }
    }

    fn stats(&self) -> Result<EngineStats, DiagramCheckError> {
        let Some(engine) = &self.engine else {
            return Err(self.unavailable());
        };
        Ok(EngineStats {
            heap_now: u64::try_from(engine.rt.memory_usage().malloc_size).unwrap_or(0),
            heap_after_last_check: self.heap_after_last_check,
            heap_limit: self.limits.memory_limit as u64,
            builds: self.builds,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard, PoisonError};
    use std::time::Duration;

    use super::*;
    use crate::ports::FailureCode;

    const TINY_CLASS: &str = "classDiagram\n  class Animal\n  class Dog\n  Animal <|-- Dog\n";

    /// The cap tests measure time and memory, so they run one at a time. They are synchronous
    /// and never `.await` while holding this.
    static CAP_TESTS: Mutex<()> = Mutex::new(());

    fn serial() -> MutexGuard<'static, ()> {
        CAP_TESTS.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn warm(limits: DiagramLimits) -> EngineState {
        let mut state = EngineState::new(limits);
        state.rebuild_if_needed();
        assert!(state.engine.is_some(), "{:?}", state.last_build_error);
        state
    }

    fn failure(answer: Result<DiagramVerdict, DiagramCheckError>) -> crate::ports::DiagramFailure {
        match answer {
            Ok(DiagramVerdict::Failed(failure)) => failure,
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn busy_loop_hits_the_time_cap_and_the_runtime_is_rebuilt() {
        let _serial = serial();
        let mut state = warm(DiagramLimits {
            time_limit: Duration::from_secs(1),
            ..DiagramLimits::default()
        });
        for _ in 0..3 {
            let started = Instant::now();
            let answer = state.run_capped("", |ctx| ctx.eval::<(), _>("(()=>{for(;;){}})()"));
            assert!(started.elapsed() < Duration::from_secs(2));
            let failure = failure(answer);
            assert_eq!(failure.code, FailureCode::TimeLimit);
            assert_eq!(
                (failure.line, failure.column, failure.snippet),
                (None, None, None)
            );
            assert!(state.engine.is_none());
            state.rebuild_if_needed();
        }
        assert_eq!(state.check(TINY_CLASS), Ok(pele::SUCCESS));
        assert_eq!(state.builds, 4);
    }

    #[test]
    fn allocation_loop_hits_the_memory_cap_and_the_runtime_is_rebuilt() {
        let _serial = serial();
        let mut state = warm(DiagramLimits::default());
        for _ in 0..3 {
            let answer = state.run_capped("", |ctx| {
                ctx.eval::<(), _>("(()=>{const a=[];for(;;)a.push(new Array(1e5).fill(0))})()")
            });
            let failure = failure(answer);
            assert_eq!(failure.code, FailureCode::MemoryLimit);
            assert_eq!(
                (failure.line, failure.column, failure.snippet),
                (None, None, None)
            );
            assert!(state.engine.is_none());
            state.rebuild_if_needed();
        }
        assert_eq!(state.check(TINY_CLASS), Ok(pele::SUCCESS));
        assert_eq!(state.builds, 4);
    }

    #[test]
    fn deep_recursion_is_a_limit_and_the_runtime_is_rebuilt() {
        let _serial = serial();
        let mut state = warm(DiagramLimits::default());
        let answer = state.run_capped("", |ctx| {
            ctx.eval::<(), _>("function f(n){return n?f(n-1)+1:0};f(1e6)")
        });
        let failure = failure(answer);
        assert_eq!(failure.code, FailureCode::Limit);
        assert_eq!(
            failure.message,
            "the diagram is nested too deeply to validate"
        );
        assert!(state.engine.is_none());
        state.rebuild_if_needed();
        assert_eq!(state.check(TINY_CLASS), Ok(pele::SUCCESS));
    }

    #[test]
    fn a_non_pele_throw_is_no_verdict_and_the_runtime_is_rebuilt() {
        let _serial = serial();
        let mut state = warm(DiagramLimits::default());
        let answer = state.run_capped("", |ctx| ctx.eval::<(), _>("throw new TypeError(\"boom\")"));
        assert_eq!(
            answer,
            Err(DiagramCheckError::NoVerdict("TypeError: boom".to_owned()))
        );
        assert!(state.engine.is_none());
        state.rebuild_if_needed();
        assert_eq!(state.check(TINY_CLASS), Ok(pele::SUCCESS));
    }

    #[test]
    fn a_thrown_null_is_a_memory_limit_and_a_thrown_string_is_not() {
        let _serial = serial();
        let mut state = warm(DiagramLimits::default());
        let answer = state.run_capped("", |ctx| ctx.eval::<(), _>("throw null"));
        assert_eq!(failure(answer).code, FailureCode::MemoryLimit);
        state.rebuild_if_needed();
        let answer = state.run_capped("", |ctx| ctx.eval::<(), _>("throw \"null\""));
        assert_eq!(answer, Err(DiagramCheckError::NoVerdict("null".to_owned())));
    }

    #[test]
    fn warm_runtime_exposes_no_io_globals() {
        let _serial = serial();
        let state = warm(DiagramLimits::default());
        let engine = state.engine.as_ref().expect("a warm engine");
        for name in [
            "std",
            "os",
            "require",
            "fetch",
            "XMLHttpRequest",
            "process",
            "Deno",
        ] {
            let kind: String = engine
                .ctx
                .with(|ctx| ctx.eval(format!("typeof {name}")))
                .expect("typeof evaluates");
            assert_eq!(kind, "undefined", "{name}");
        }
    }

    #[test]
    fn agent_source_is_data_not_code() {
        let _serial = serial();
        let mut state = warm(DiagramLimits::default());
        let _ = state.check("classDiagram\n  class A\n\");globalThis.pwned=1;//");
        let engine = state.engine.as_ref().expect("the runtime is kept");
        let kind: String = engine
            .ctx
            .with(|ctx| ctx.eval("typeof globalThis.pwned"))
            .expect("typeof evaluates");
        assert_eq!(kind, "undefined");
    }
}
