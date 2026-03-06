fn main() {
    let (config, _) = tymnl::config::Config::load("example-config/tymnl.yml").unwrap();
    dbg!(config);
}
