//! Manual probe: resolve phrases against the real registry on this machine.
//!
//! Exists because unit tests use a synthetic registry, and the interesting
//! failures come from real application names. Prints resolutions without
//! launching anything unless --launch is passed.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let do_launch = args.iter().any(|a| a == "--launch");
    let phrases: Vec<&String> = args.iter().filter(|a| *a != "--launch").collect();
    let reg = app_command::discover();
    eprintln!("registry: {} applications", reg.apps().len());
    if phrases.is_empty() {
        for app in reg.apps() {
            println!("  {}  [{}]", app.name, app.launch_id);
        }
        return;
    }
    for p in phrases {
        match app_command::parse(p, &reg) {
            Ok(app_command::AppCommand::Launch(app)) => {
                println!("{p:40} -> LAUNCH {}", app.name);
                if do_launch {
                    match app_command::launch(&app) {
                        Ok(()) => println!("{:40}    launched ok", ""),
                        Err(e) => println!("{:40}    FAILED: {e}", ""),
                    }
                }
            }
            Err(r) => println!("{p:40} -> refuse: {}", r.say()),
        }
    }
}
