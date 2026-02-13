mod domain;
mod error;

fn main() {
    let name = domain::BranchName::new("feature/auth");
    let dir = name.to_dir_name();
    eprintln!("wt-core: skeleton (example dir: {dir})");
}
