/// The exact global usage bytes of the installed one-executable CLI.
///
/// This is an independent behavioural oracle. It does not import or derive
/// the production usage constant, so any production change to the usage text
/// fails the exact-byte assertions that compare against it.
pub const EXPECTED_USAGE: &[u8] = b"Usage:\n  orna --version\n  orna server run\n  orna server upgrade\n  orna server backend-shell\n  orna source check <file.orna>\n  orna source apply <file.orna>\n  orna security grant-execute <canonical-function-id>\n  orna raw-call <canonical-function-id>\n  orna raw-call <canonical-function-id> <canonical-parameter-id>\n  orna [--runtime <family>] invoke <qualified-name | canonical-function-id> [options]\n  orna state get <root-function-id> [options]\n  orna state set <root-function-id> [options]\n  orna inspect <invocation-id> [options]\n";
