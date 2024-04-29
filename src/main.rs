use crate::cont::{
    ContRepositoryObject, ContRepositoryObjectOnBlob, ContRepositoryObjectOnTree,
    ContRepositoryObjectOnTreeEntries,
};
use anyhow::{bail, Context as _};
use camino::Utf8PathBuf;
use clap::Parser;
use graphql_client::GraphQLQuery;
use serde::{Deserialize, Serialize};
use start::{
    StartRepositoryObject, StartRepositoryObjectOnBlob, StartRepositoryObjectOnTree,
    StartRepositoryObjectOnTreeEntries,
};

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
        let ureq = ureq::post(&self.endpoint);
        let ureq = match self.token.as_deref() {
            Some(it) => ureq.set("Authorization", &format!("Bearer {}", it)),
            None => ureq,
        };
        let graphql_client::Response {
            data,
            errors,
            extensions: _,
        } = ureq
            .send_json(T::build_query(params))?
            .into_json::<graphql_client::Response<T::ResponseData>>()?;
        if let Some(errors) = errors {
            bail!("errors: {:?}", errors)
        }
        data.context("no response")
    }
}

#[derive(Parser)]
struct Args {
    owner: String,
    name: String,
    commit_ish: String,
    path: Utf8PathBuf,
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

#[derive(Debug)]
enum Root {
    File(String),
    Folder(Vec<Node>),
}

#[derive(Debug)]
enum Node {
    File { name: String, contents: String },
    Folder { name: String, contents: Vec<Self> },
}

fn fill(
    client: &Client,
    repo_name: &str,
    repo_owner: &str,
    name: String,
    oid: GitObjectID,
) -> anyhow::Result<Node> {
    let resp = client.call::<Cont>(cont::Variables {
        repo_name: repo_name.into(),
        repo_owner: repo_owner.into(),
        oid,
    })?;
    let node = match resp
        .repository
        .context("no repository")?
        .object
        .context("no object")?
    {
        ContRepositoryObject::Blob(ContRepositoryObjectOnBlob { text }) => Node::File {
            name,
            contents: text.context("only text files are supported")?,
        },
        ContRepositoryObject::Tree(ContRepositoryObjectOnTree { entries }) => Node::Folder {
            name,
            contents: entries
                .into_iter()
                .flatten()
                .map(|ContRepositoryObjectOnTreeEntries { name, oid }| {
                    fill(client, repo_name, repo_owner, name, oid)
                })
                .collect::<Result<_, _>>()?,
        },
        other => bail!("unexpected object: {:?}", other),
    };

    Ok(node)
}

fn main() -> anyhow::Result<()> {
    let Args {
        endpoint,
        token,
        name: repo_name,
        owner: repo_owner,
        commit_ish,
        path,
    } = Args::parse();
    let client = Client { endpoint, token };
    let path = match path.is_absolute() {
        true => path
            .components()
            .rev()
            .skip(1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
        false => path,
    };

    let start = client
        .call::<Start>(start::Variables {
            repo_owner: repo_owner.clone(),
            repo_name: repo_name.clone(),
            rev_parse: format!("{}:{}", commit_ish, path),
        })?
        .repository
        .context("no repository")?;
    let root = match start.object.context("no object")? {
        StartRepositoryObject::Blob(StartRepositoryObjectOnBlob { text }) => {
            Root::File(text.context("blob is binary")?)
        }
        StartRepositoryObject::Tree(StartRepositoryObjectOnTree { entries }) => Root::Folder(
            entries
                .into_iter()
                .flatten()
                .map(|StartRepositoryObjectOnTreeEntries { name, oid }| {
                    fill(&client, &repo_name, &repo_owner, name, oid)
                })
                .collect::<Result<_, _>>()?,
        ),
        other => bail!("expected blob or tree, not {:?}", other),
    };
    dbg!(root);

    Ok(())
}
