fn main() {
    if std::env::args()
        .skip(1)
        .any(|argument| argument == "--restore-and-exit")
    {
        std::process::exit(controwly_lib::run_restore_cli());
    }
    std::process::exit(controwly_lib::run());
}
