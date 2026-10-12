fn main() {
    let path = std::env::args_os()
        .nth(1)
        .expect("usage: validate-package ZIP");
    let bytes = std::fs::read(path).expect("read package");
    match nova_providers::AddonPackage::parse(&bytes) {
        Ok(package) => println!(
            "{} {} {}",
            package.id(),
            package.version(),
            package.sha256()
        ),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
