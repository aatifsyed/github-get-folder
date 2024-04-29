use std::{borrow::Cow, fs};

use self::{
    cont::{
        ContRepositoryObject, ContRepositoryObjectOnBlob, ContRepositoryObjectOnTree,
        ContRepositoryObjectOnTreeEntries,
    },
    start::{
        StartRepositoryObject, StartRepositoryObjectOnBlob, StartRepositoryObjectOnTree,
        StartRepositoryObjectOnTreeEntries,
    },
};
use anyhow::{bail, Context as _};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Parser;
use graphql_client::GraphQLQuery;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct GitObjectID(String);

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "schema.graphql",
    query_path = "queries.graphql",
    response_derives = "Debug"
)]
pub struct Start;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "schema.graphql",
    query_path = "queries.graphql",
    response_derives = "Debug"
)]
pub struct Cont;

impl Client {
    pub fn call<T: GraphQLQuery>(&self, params: T::Variables) -> anyhow::Result<T::ResponseData> {
        let query = T::build_query(params);
        let ureq = ureq::post(&self.endpoint);
        let resp = match self.token.as_deref() {
            Some(it) => ureq.set("Authorization", &format!("Bearer {}", it)),
            None => ureq,
        }
        .send_json(query);
        let graphql_client::Response {
            data,
            errors,
            extensions: _,
        } = match resp {
            Ok(it) => it.into_json::<graphql_client::Response<T::ResponseData>>()?,
            Err(it @ ureq::Error::Transport(_)) => Err(it)?,
            Err(ureq::Error::Status(_, resp)) => {
                let msg = format!("{:?}", resp);
                let body = resp.into_string().unwrap_or_default();
                bail!("{}:\n{}", msg, body)
            }
        };
        if let Some(errors) = errors {
            bail!(
                "{}",
                errors
                    .iter()
                    .map(|it| it.to_string())
                    .fold(String::new(), |acc, el| acc + &el + "\n")
            )
        }
        data.context("no response")
    }
}

fn get(
    client: &Client,
    repo_name: &str,
    repo_owner: &str,
    local_path: Cow<Utf8Path>,
    oid: GitObjectID,
) -> anyhow::Result<()> {
    match client
        .call::<Cont>(cont::Variables {
            repo_name: repo_name.into(),
            repo_owner: repo_owner.into(),
            oid,
        })?
        .repository
        .and_then(|it| it.object)
        .context("incomplete response")?
    {
        ContRepositoryObject::Blob(ContRepositoryObjectOnBlob { text }) => {
            println!("blob {}", local_path);
            fs::write(
                local_path.as_std_path(),
                text.context("binary blobs are not supported")?,
            )?;
        }
        ContRepositoryObject::Tree(ContRepositoryObjectOnTree { entries }) => {
            println!("tree {}", local_path);
            fs::create_dir_all(local_path.as_std_path())?;
            for ContRepositoryObjectOnTreeEntries { name, oid } in entries.into_iter().flatten() {
                get(
                    client,
                    repo_name,
                    repo_owner,
                    local_path.join(name).into(),
                    oid,
                )?
            }
        }
        ContRepositoryObject::Commit => bail!("unexpected `commit` object"),
        ContRepositoryObject::Tag => bail!("unexpected `tag` object"),
    }
    Ok(())
}

#[derive(Parser)]
struct Args {
    owner: String,
    name: String,
    #[arg(default_value = "HEAD")]
    commit_ish: String,
    #[arg(default_value = "/")]
    remote_path: Utf8PathBuf,
    #[arg(default_value = ".")]
    local_path: Utf8PathBuf,
    #[arg(long, default_value = "https://api.github.com/graphql")]
    endpoint: String,
    #[arg(long, env("GITHUB_TOKEN"))]
    token: Option<String>,
}

#[derive(Debug)]
pub struct Client {
    endpoint: String,
    token: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let Args {
        owner: repo_owner,
        name: repo_name,
        commit_ish,
        remote_path,
        local_path,
        endpoint,
        token,
    } = Args::parse();
    let client = Client { endpoint, token };
    let remote_path = match remote_path.is_absolute() {
        true => remote_path
            .components()
            .rev()
            .skip(1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
        false => remote_path,
    };

    let start = client
        .call::<Start>(start::Variables {
            repo_owner: repo_owner.clone(),
            repo_name: repo_name.clone(),
            rev_parse: format!("{}:{}", commit_ish, remote_path),
        })?
        .repository
        .context("no repository")?;
    match start.object.context("no object")? {
        StartRepositoryObject::Blob(StartRepositoryObjectOnBlob { text }) => {
            fs::write(local_path, text.context("binary blobs are not supported")?)?;
        }
        StartRepositoryObject::Tree(StartRepositoryObjectOnTree { entries }) => {
            fs::create_dir_all(local_path.as_std_path())?;
            for StartRepositoryObjectOnTreeEntries { name, oid } in entries.into_iter().flatten() {
                get(
                    &client,
                    &repo_name,
                    &repo_owner,
                    local_path.join(name).into(),
                    oid,
                )?;
            }
        }
        StartRepositoryObject::Commit => bail!("unexpected `commit` object"),
        StartRepositoryObject::Tag => bail!("unexpected `tag` object"),
    }

    Ok(())
}
