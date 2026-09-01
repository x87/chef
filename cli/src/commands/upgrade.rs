use crate::handlers;

/// Update the chef binary itself. No inputs to parse or validate - the
/// request (`check` vs real upgrade, output mode) goes straight to the
/// upgrade handler.
pub fn run(check: bool, json: bool) -> crate::Result<()> {
    handlers::upgrade::run(check, json)
}
