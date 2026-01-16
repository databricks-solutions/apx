pub mod build;
pub mod dev;
pub mod init;
pub mod __generate_openapi;

pub fn run_cli<F>(f: F) -> i32
where
    F: FnOnce() -> Result<(), String>,
{
    match f() {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}
