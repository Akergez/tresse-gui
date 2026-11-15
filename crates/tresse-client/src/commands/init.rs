use crate::Repo;
use crate::cas::open_local_store;
use crate::config::TresseConfig;

pub fn run(repo: &Repo) -> Result<(), Box<dyn std::error::Error>> {
    let vault = repo.vault();
    vault.mkdir_all(".tresse")?;
    if vault.metadata("tresse.toml")?.is_none() {
        TresseConfig::init(vault.as_ref())?;
    }

    open_local_store(repo)?;

    println!("initialized tresse repo at {}", repo.label());
    Ok(())
}
