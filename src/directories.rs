//! Read-only directory navigation for the authenticated operator.
use anyhow::{Context, Result, ensure};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::path::Path;
use tokio_tungstenite::tungstenite::Message;

pub fn listing(path: &str, value: &Value) -> Result<Value> {
    ensure!(path.starts_with('/'), "Executor returned a non-absolute directory");
    let rows = value["entries"].as_array().context("Executor returned no directory entries")?;
    let mut names: Vec<_> = rows.iter().filter(|row|row["isDirectory"]==true)
        .filter_map(|row|row["fileName"].as_str())
        .filter(|name| !name.is_empty() && !matches!(*name,"."|"..") && !name.contains('/') && !name.contains('\0'))
        .collect();
    names.sort_unstable(); names.dedup();
    let truncated = names.len()>500 || value["truncated"]==true;
    names.truncate(500);
    let entries:Vec<_> = names.into_iter().map(|name|json!({"name":name,"path":format!("{}/{name}",path.trim_end_matches('/'))})).collect();
    Ok(json!({"path":path,"parent":Path::new(path).parent().and_then(Path::to_str),"entries":entries,"truncated":truncated}))
}

pub async fn local(path: &str) -> Result<Value> {
    let canonical = tokio::fs::canonicalize(path).await.context("Cannot resolve directory")?;
    let path = canonical.to_str().context("Directory path is not UTF-8")?;
    let mut reader = tokio::fs::read_dir(&canonical).await.context("Cannot read directory")?;
    let mut entries=Vec::new();
    let mut scanned=0;
    let mut truncated=false;
    while let Some(entry)=reader.next_entry().await? {
        scanned+=1;
        if scanned>10000 { truncated=true; break; }
        let kind=entry.file_type().await?;
        let directory=kind.is_dir() || (kind.is_symlink() && tokio::fs::metadata(entry.path()).await.is_ok_and(|m|m.is_dir()));
        if directory {
            if let Some(name)=entry.file_name().to_str() { entries.push(json!({"fileName":name,"isDirectory":true})); }
            else { truncated=true; }
        }
    }
    listing(path,&json!({"entries":entries,"truncated":truncated}))
}

pub async fn remote(url: &str, path: &str) -> Result<Value> {
    let (mut socket,_) = tokio_tungstenite::connect_async(url).await.context("Cannot connect to executor")?;
    let mut canonical=String::new();
    let mut result=Value::Null;
    for (id, method) in [(1,"initialize"),(2,"fs/canonicalize"),(3,"fs/readDirectory")] {
        let params=match id {
            1=>json!({"clientName":"demodex-directory-picker"}),
            2=>json!({"path":crate::ssh::sftp::path_uri(path)}),
            _=>json!({"path":crate::ssh::sftp::path_uri(&canonical)}),
        };
        socket.send(Message::Text(json!({"id":id,"method":method,"params":params}).to_string().into())).await?;
        loop {
            let message=socket.next().await.context("Executor closed the connection")??;
            if let Message::Text(raw)=message {
                let response:Value=serde_json::from_str(&raw)?;
                if response["id"]!=id { continue; }
                ensure!(response["error"].is_null(), "Directory operation failed: {}",response["error"]);
                result=response["result"].clone();
                break;
            }
        }
        if id==1 { socket.send(Message::Text(json!({"method":"initialized","params":{}}).to_string().into())).await?; }
        if id==2 { canonical=crate::ssh::sftp::uri_path(result["path"].as_str().context("Executor returned no canonical path")?)?; }
    }
    socket.close(None).await?;
    listing(&canonical,&result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn listing_filters_sorts_and_bounds_untrusted_names() {
        let data=json!({"entries":[{"fileName":"z","isDirectory":true},{"fileName":"..","isDirectory":true},{"fileName":"bad/name","isDirectory":true},{"fileName":"🦆 space","isDirectory":true},{"fileName":"file","isDirectory":false}]});
        let value=listing("/",&data).unwrap();
        assert!(value["parent"].is_null());
        assert_eq!(value["entries"].as_array().unwrap().len(),2);
        assert_eq!(value["entries"][1]["path"],"/🦆 space");
    }
    #[tokio::test]
    async fn local_lists_directories_and_resolves_symlinks() {
        let root=std::env::temp_dir().join(format!("demodex-picker-{}",uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(root.join("🦆 space")).await.unwrap();
        tokio::fs::write(root.join("file"),b"not a directory").await.unwrap();
        let data=local(root.to_str().unwrap()).await.unwrap();
        assert_eq!(data["entries"].as_array().unwrap().len(),1);
        assert!(local(root.join("missing").to_str().unwrap()).await.is_err());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
