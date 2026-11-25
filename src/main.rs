mod lyfta;
mod tools;

use std::{
    env,
    error::Error,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use rmcp::{ServiceExt, transport::stdio};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let dotenv_path = dotenv_path()?;
    let key = api_key(&dotenv_path)?;
    let server = tools::LyftaServer::new(lyfta::LyftaClient::new(key)?)?;
    server.serve(stdio()).await?.waiting().await?;
    Ok(())
}

fn dotenv_path() -> Result<PathBuf, &'static str> {
    let mut args = env::args_os().skip(1);
    match (args.next(), args.next(), args.next()) {
        (None, None, None) => Ok(".env".into()),
        (Some(flag), Some(path), None) if flag == "--env-file" => Ok(path.into()),
        _ => Err("Usage: lyfta-mcp [--env-file PATH]"),
    }
}

fn api_key(path: &Path) -> Result<Option<String>, &'static str> {
    if let Ok(key) = env::var("LYFTA_API_KEY") {
        return Ok(Some(key));
    }

    let entries = match dotenvy::from_path_iter(path) {
        Ok(entries) => entries,
        Err(dotenvy::Error::Io(err)) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Could not read the Lyfta environment file."),
    };
    for entry in entries {
        let (name, value) = entry.map_err(|_| "Invalid entry in the Lyfta environment file.")?;
        if name == "LYFTA_API_KEY" {
            return Ok(Some(value));
        }
    }
    Ok(None)
}
