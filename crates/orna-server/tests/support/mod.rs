/// The exact global usage bytes of the local CLI.
///
/// This is an independent behavioural oracle. It does not import or derive
/// the production usage constant, so any production change to the usage text
/// fails the exact-byte assertions that compare against it.
pub const EXPECTED_USAGE: &[u8] = b"Usage:\n  orna [OPTIONS] [URI]\n  orna [OPTIONS] [URI] invoke <function> [OPTIONS]\n  orna [OPTIONS] -d\n  orna --help\n  orna --version\n\nCommands:\n  invoke       Run one stored function.\n  repl         Open the function-backed REPL.\n  source       Check or apply Orna source.\n  inspect      Inspect a completed invocation.\n  raw-call     Make a low-level local call.\n  orna raw-call <canonical-function-id>\n\nOptions:\n  --db <URI>   Select the database URI.\n  -d, --daemon Run the local server in the foreground.\n  --runtime <family>  Select tty or qt for invoke.\n  --color <auto|always|never>  Control terminal colour.\n  -h, --help   Show help.\n  -V, --version Show the version.\n";
