//! Camera worker. Separate binary so the SHA engine does not link ONNX Runtime.
mod watch;

fn main() {
    if let Err(e) = watch::main() {
        eprintln!("cw: {e}");
        std::process::exit(1);
    }
}
