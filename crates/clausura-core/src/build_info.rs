pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const COMMIT: &str = env!("CLAUSURA_GIT_COMMIT");
pub const BUILD_DATE: &str = env!("CLAUSURA_BUILD_DATE");

pub const VERSION_FULL: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (commit: ",
    env!("CLAUSURA_GIT_COMMIT"),
    ", built: ",
    env!("CLAUSURA_BUILD_DATE"),
    ")"
);

pub fn version_string() -> String {
    format!(
        "clausura {} (commit: {}, built: {})",
        VERSION, COMMIT, BUILD_DATE
    )
}
