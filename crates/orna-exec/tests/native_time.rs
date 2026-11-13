//! The `core.time` zone natives over lowered source, against the old evaluator's outcomes: local times resolved in
//! Europe/London across a winter and a summer offset, the fold and the gap of the spring and autumn transitions with
//! each policy, and the `ORNA-EVAL-VALUE` refusals of an unknown zone, a bad policy and a rejected gap or fold.

use num_bigint::BigInt;
use std::cell::Cell;

use orna_exec::{
    ActivationContext, ExecError, ExecLimits, Host, HostFailure, HostInstant, HostManifest,
    RowSource, StoreHost, SysSlot, TableStore, Value, run_entry, run_entry_with_host,
};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;
use orna_value::{Raw, Snapshot};

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_collection_items: 2_048,
    max_string_bytes: 1 << 20,
    max_integer_digits: 1_024,
};

const HEAD: &str = "use core.time;\n";

fn lower_one(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &parsed.value,
        path: ModulePath::new([Name::new("main")]),
    }]);
    let user = ModulePath::new([Name::new("main")]);
    let diagnostics: Vec<String> = program
        .modules
        .iter()
        .filter(|module| module.path == user)
        .flat_map(|module| {
            module
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{diagnostic:?}"))
        })
        .collect();
    assert!(
        diagnostics.is_empty(),
        "lowering diagnostics: {diagnostics:?}"
    );
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

/// The program `pub fn run(): <result_type> = <expression>;` behind the `core.time` import.
fn source(result_type: &str, expression: &str) -> String {
    format!("{HEAD}pub fn run(): {result_type} = {expression};\n")
}

fn run_with(source: &str, limits: ExecLimits) -> Result<Value, ExecError> {
    let program = lower_one(source);
    let run_def: DefId = program
        .modules
        .iter()
        .flat_map(|module| module.items.iter().copied())
        .find(|def| {
            matches!(program.defs[*def].kind, DefKind::Fn(_))
                && program.defs[*def].name.as_str() == "run"
        })
        .expect("the source defines `run`");
    run_entry(&program, run_def, limits).result
}

fn run(result_type: &str, expression: &str) -> Result<Value, ExecError> {
    run_with(&source(result_type, expression), LIMITS)
}

fn instant(unix_seconds: i64) -> Value {
    Value::Instant {
        unix_seconds,
        nanosecond: 0,
    }
}

/// 2024-01-15T12:00:00 GMT, which is 2024-01-15T12:00:00Z.
const WINTER_NOON_UTC: i64 = 1_705_320_000;
/// 2024-07-01T12:00:00 BST, which is 2024-07-01T11:00:00Z.
const SUMMER_NOON_UTC: i64 = 1_719_831_600;
/// 2024-10-27T01:30 occurs twice in London: at 00:30Z under BST and at 01:30Z under GMT.
const FOLD_EARLIER_UTC: i64 = 1_729_989_000;
const FOLD_LATER_UTC: i64 = 1_729_992_600;
/// 2024-03-31T01:30 does not exist in London: the clocks move from 01:00Z GMT to 02:00 BST.
/// Shifting forward applies the offset before the gap (0), shifting backward applies the one after it (+1h).
const GAP_SHIFT_FORWARD_UTC: i64 = 1_711_848_600;
const GAP_SHIFT_BACKWARD_UTC: i64 = 1_711_845_000;

fn resolve(local: &str, zone: &str, ambiguous: &str, gap: &str) -> String {
    format!("time.resolve_local(\"{local}\", \"{zone}\", \"{ambiguous}\", {gap})")
}

#[test]
fn resolve_local_uses_the_offset_in_force_at_the_local_time() {
    assert_eq!(
        run(
            "Instant",
            &resolve("2024-01-15T12:00:00", "Europe/London", "reject", "null")
        ),
        Ok(instant(WINTER_NOON_UTC))
    );
    assert_eq!(
        run(
            "Instant",
            &resolve("2024-07-01T12:00:00", "Europe/London", "reject", "null")
        ),
        Ok(instant(SUMMER_NOON_UTC))
    );
}

#[test]
fn offset_at_reports_the_zone_offset_in_seconds() {
    let summer = resolve("2024-07-01T12:00:00", "Europe/London", "reject", "null");
    let winter = resolve("2024-01-15T12:00:00", "Europe/London", "reject", "null");
    assert_eq!(
        run(
            "Int",
            &format!("time.offset_at({summer}, \"Europe/London\")")
        ),
        Ok(Value::Int(BigInt::from(3_600)))
    );
    assert_eq!(
        run(
            "Int",
            &format!("time.offset_at({winter}, \"Europe/London\")")
        ),
        Ok(Value::Int(BigInt::from(0)))
    );
}

#[test]
fn an_ambiguous_local_time_follows_the_requested_fold() {
    assert_eq!(
        run(
            "Instant",
            &resolve("2024-10-27T01:30:00", "Europe/London", "earlier", "null")
        ),
        Ok(instant(FOLD_EARLIER_UTC))
    );
    assert_eq!(
        run(
            "Instant",
            &resolve("2024-10-27T01:30:00", "Europe/London", "later", "null")
        ),
        Ok(instant(FOLD_LATER_UTC))
    );
    assert_eq!(
        run(
            "Instant",
            &resolve("2024-10-27T01:30:00", "Europe/London", "reject", "null")
        ),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn a_nonexistent_local_time_follows_the_gap_policy() {
    let local = "2024-03-31T01:30:00";
    assert_eq!(
        run(
            "Instant",
            &resolve(local, "Europe/London", "reject", "\"shift_forward\"")
        ),
        Ok(instant(GAP_SHIFT_FORWARD_UTC))
    );
    assert_eq!(
        run(
            "Instant",
            &resolve(local, "Europe/London", "reject", "\"shift_backward\"")
        ),
        Ok(instant(GAP_SHIFT_BACKWARD_UTC))
    );
    assert_eq!(
        run(
            "Instant",
            &resolve(local, "Europe/London", "reject", "\"reject\"")
        ),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        run(
            "Instant",
            &resolve(local, "Europe/London", "reject", "null")
        ),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn an_unknown_zone_or_policy_is_refused() {
    assert_eq!(
        run(
            "Instant",
            &resolve("2024-01-15T12:00:00", "Mars/Olympus", "reject", "null")
        ),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        run(
            "Instant",
            &resolve("2024-01-15T12:00:00", "Europe/London", "sometimes", "null")
        ),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

/// A host whose clock moves one second on every read, so a run that asked the host again would see a later instant
/// than its first. Its entropy counts up from one, which makes `uuid7` deterministic.
struct FixtureHost {
    inner: StoreHost,
    clock: Cell<i64>,
    counter: u8,
}

/// The unix second of the fixture host's first clock read.
const FIXTURE_START: i64 = 1_700_000_000;

impl FixtureHost {
    fn new() -> Self {
        Self {
            inner: StoreHost::empty(),
            clock: Cell::new(FIXTURE_START),
            counter: 0,
        }
    }
}

impl Host for FixtureHost {
    fn manifest(&self) -> &HostManifest {
        self.inner.manifest()
    }

    fn store(&mut self) -> &mut dyn TableStore {
        self.inner.store()
    }

    fn store_at(&mut self, at: &Snapshot) -> Result<Box<dyn TableStore>, HostFailure> {
        self.inner.store_at(at)
    }

    fn now(&self) -> HostInstant {
        let seconds = self.clock.get();
        self.clock.set(seconds + 1);
        HostInstant {
            unix_seconds: seconds,
            nanos: 500_000_000,
        }
    }

    fn entropy(&mut self, out: &mut [u8]) -> Result<(), HostFailure> {
        for byte in out {
            self.counter = self.counter.wrapping_add(1);
            *byte = self.counter;
        }
        Ok(())
    }

    fn activation(&self) -> Option<&ActivationContext> {
        self.inner.activation()
    }

    fn sys_call(&mut self, slot: SysSlot, args: Vec<Raw>) -> Result<Raw, HostFailure> {
        self.inner.sys_call(slot, args)
    }

    fn sys_static(&mut self, slot: SysSlot) -> Result<Raw, HostFailure> {
        self.inner.sys_static(slot)
    }

    fn sys_relation(
        &mut self,
        slot: SysSlot,
        at: Option<&Snapshot>,
    ) -> Result<Box<dyn RowSource>, HostFailure> {
        self.inner.sys_relation(slot, at)
    }
}

fn run_on(host: &mut dyn Host, source: &str) -> Result<Value, ExecError> {
    let program = lower_one(source);
    run_entry_with_host(&program, entry_def(&program), LIMITS, host).result
}

#[test]
fn now_is_one_instant_for_the_whole_activation() {
    let mut host = FixtureHost::new();
    assert_eq!(
        run_on(&mut host, &source("Bool", "now() == now()")),
        Ok(Value::Bool(true))
    );
}

#[test]
fn now_is_the_instant_the_host_read_when_the_activation_started() {
    let mut host = FixtureHost::new();
    assert_eq!(
        run_on(&mut host, &source("Instant", "now()")),
        Ok(Value::Instant {
            unix_seconds: FIXTURE_START,
            nanosecond: 500_000_000,
        })
    );
}

#[test]
fn uuid7_is_a_version_7_uuid_of_the_activation_time_and_host_entropy() {
    let mut host = FixtureHost::new();
    let millis =
        u64::try_from(FIXTURE_START).expect("the fixture start is after the epoch") * 1_000 + 500;
    let mut expected = millis.to_be_bytes()[2..].to_vec();
    expected.extend_from_slice(&[0x71, 0x02, 0x83, 4, 5, 6, 7, 8, 9, 10]);
    let expected: [u8; 16] = expected.try_into().expect("a UUID is sixteen bytes");
    assert_eq!(
        run_on(&mut host, &source("Uuid", "uuid7()")),
        Ok(Value::Uuid(expected))
    );
    assert_eq!(expected[6] >> 4, 7, "the version nibble is 7");
    assert_eq!(expected[8] & 0xc0, 0x80, "the variant bits are 10");
}

/// The zero-parameter `run` of the lowered `program`.
fn entry_def(program: &Program) -> DefId {
    program
        .modules
        .iter()
        .flat_map(|module| module.items.iter().copied())
        .find(|def| {
            matches!(program.defs[*def].kind, DefKind::Fn(_))
                && program.defs[*def].name.as_str() == "run"
        })
        .expect("the source defines `run`")
}
