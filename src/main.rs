fn main() {
    if let Err(err) = marian::serve::run() {
        eprintln!("Marian stopped: {err}");
        std::process::exit(1);
    }
}
