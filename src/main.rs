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
use futures::{stream, StreamExt as _, TryStreamExt as _};
use graphql_client::GraphQLQuery;
use itertools::Itertools as _;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

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

pub struct Client {
    inner: reqwest::Client,
    url: String,
    token: Option<String>,
}

impl Client {
    async fn call<T: GraphQLQuery>(&self, params: T::Variables) -> anyhow::Result<T::ResponseData> {
        let builder = self.inner.post(&self.url).json(&T::build_query(params));
        let response = match &self.token {
            Some(it) => builder.bearer_auth(it),
            None => builder,
        }
        .send()
        .await?;

        let graphql_client::Response {
            data,
            errors,
            extensions: _,
        } = match response.error_for_status_ref() {
            Ok(_) => response.json().await?,
            Err(e) => {
                bail!("{}\n\n{}", e, response.text().await?)
            }
        };

        if errors.as_ref().is_some_and(|it| !it.is_empty()) {
            bail!("query errors: {}", errors.into_iter().flatten().join(", "))
        }

        data.context("query response has no `data` member")
    }
}

async fn get(
    client: &Client,
    repo_name: &str,
    repo_owner: &str,
    local_path: Cow<'_, Utf8Path>,
    oid: GitObjectID,
) -> anyhow::Result<()> {
    match client
        .call::<Cont>(cont::Variables {
            repo_name: repo_name.into(),
            repo_owner: repo_owner.into(),
            oid,
        })
        .await?
        .repository
        .and_then(|it| it.object)
        .context("incomplete response")?
    {
        ContRepositoryObject::Blob(ContRepositoryObjectOnBlob { text }) => {
            println!("blob {}", local_path);
            tokio::fs::write(
                local_path.as_std_path(),
                text.context("binary blobs are not supported")?,
            )
            .await?;
        }
        ContRepositoryObject::Tree(ContRepositoryObjectOnTree { entries }) => {
            println!("tree {}", local_path);
            tokio::fs::create_dir_all(local_path.as_std_path()).await?;
            let concurrency = entries
                .as_ref()
                .map(Vec::len)
                .unwrap_or_default()
                .saturating_add(1);
            stream::iter(entries.into_iter().flatten())
                .map(|ContRepositoryObjectOnTreeEntries { name, oid }| {
                    get(
                        client,
                        repo_name,
                        repo_owner,
                        local_path.join(name).into(),
                        oid,
                    )
                })
                .buffer_unordered(concurrency)
                .try_collect::<()>()
                .await?;
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

fn main() -> anyhow::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(_main())
}

const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

async fn _main() -> anyhow::Result<()> {
    let Args {
        owner: repo_owner,
        name: repo_name,
        commit_ish,
        remote_path,
        local_path,
        endpoint,
        token,
    } = Args::parse();
    let client = Client {
        inner: reqwest::Client::builder().user_agent(USER_AGENT).build()?,
        url: endpoint,
        token,
    };
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
        })
        .await?
        .repository
        .context("no `repository` member")?;
    match start.object.context("no `object` member")? {
        StartRepositoryObject::Blob(StartRepositoryObjectOnBlob { text }) => {
            tokio::fs::write(local_path, text.context("binary blobs are not supported")?).await?;
        }
        StartRepositoryObject::Tree(StartRepositoryObjectOnTree { entries }) => {
            tokio::fs::create_dir_all(local_path.as_std_path()).await?;
            let concurrency = entries
                .as_ref()
                .map(Vec::len)
                .unwrap_or_default()
                .saturating_add(1);
            stream::iter(entries.into_iter().flatten())
                .map(|StartRepositoryObjectOnTreeEntries { name, oid }| {
                    get(
                        &client,
                        &repo_name,
                        &repo_owner,
                        local_path.join(name).into(),
                        oid,
                    )
                })
                .buffer_unordered(concurrency)
                .try_collect::<()>()
                .await?;
        }
        StartRepositoryObject::Commit => bail!("unexpected `commit` object"),
        StartRepositoryObject::Tag => bail!("unexpected `tag` object"),
    }

    Ok(())
}
