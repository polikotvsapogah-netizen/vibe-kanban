use std::path::PathBuf;

use db::models::{
    pull_request::PullRequest,
    repo::{Repo, RepoError},
    workspace::Workspace,
    workspace_repo::WorkspaceRepo,
};
use git::{GitCliError, GitServiceError};
use git_host::{CreatePrRequest, GitHostError, GitHostProvider, GitHostService};
use thiserror::Error;

use crate::services::container::{ContainerError, ContainerService};

#[derive(Debug)]
pub enum AutoPrOutcome {
    Created { url: String },
    Skipped { reason: String },
}

#[derive(Debug, Error)]
pub enum AutoPrError {
    #[error(transparent)]
    Container(#[from] ContainerError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Repo(#[from] RepoError),
    #[error(transparent)]
    Git(#[from] GitServiceError),
    #[error(transparent)]
    GitHost(#[from] GitHostError),
}

pub async fn try_auto_create_pr(
    container: &(impl ContainerService + ?Sized),
    workspace: &Workspace,
    title: &str,
    body: Option<&str>,
) -> Result<AutoPrOutcome, AutoPrError> {
    let pool = &container.db().pool;

    let workspace_repos = WorkspaceRepo::find_by_workspace_id(pool, workspace.id).await?;
    if workspace_repos.len() != 1 {
        return Ok(AutoPrOutcome::Skipped {
            reason: "auto_create_pr only supports single-repo workspaces".to_string(),
        });
    }

    let workspace_repo = &workspace_repos[0];
    let repo = Repo::find_by_id(pool, workspace_repo.repo_id)
        .await?
        .ok_or(RepoError::NotFound)?;

    let container_ref = container.ensure_container_exists(workspace).await?;
    let workspace_path = PathBuf::from(container_ref);
    let worktree_path = workspace_path.join(&repo.name);

    let git = container.git();
    let target_branch = workspace_repo.target_branch.clone();
    let push_remote = git.resolve_remote_for_branch(&repo.path, &workspace.branch)?;

    let (target_remote, base_branch) =
        match git.get_remote_from_branch_name(&repo.path, &target_branch) {
            Ok(remote) => {
                let branch = target_branch
                    .strip_prefix(&format!("{}/", remote.name))
                    .unwrap_or(&target_branch);
                (remote, branch.to_string())
            }
            Err(_) => (push_remote.clone(), target_branch.clone()),
        };

    match git.check_remote_branch_exists(&repo.path, &target_remote.url, &base_branch) {
        Ok(true) => {}
        Ok(false) => {
            return Ok(AutoPrOutcome::Skipped {
                reason: format!("target branch '{}' was not found", target_branch),
            });
        }
        Err(GitServiceError::GitCLI(GitCliError::AuthFailed(_))) => {
            return Ok(AutoPrOutcome::Skipped {
                reason: "git CLI is not logged in".to_string(),
            });
        }
        Err(GitServiceError::GitCLI(GitCliError::NotAvailable)) => {
            return Ok(AutoPrOutcome::Skipped {
                reason: "git CLI is not installed".to_string(),
            });
        }
        Err(e) => return Err(e.into()),
    }

    if let Err(e) = git.push_to_remote(&worktree_path, &workspace.branch, false) {
        match e {
            GitServiceError::GitCLI(GitCliError::AuthFailed(_)) => {
                return Ok(AutoPrOutcome::Skipped {
                    reason: "git CLI is not logged in".to_string(),
                });
            }
            GitServiceError::GitCLI(GitCliError::NotAvailable) => {
                return Ok(AutoPrOutcome::Skipped {
                    reason: "git CLI is not installed".to_string(),
                });
            }
            other => return Err(other.into()),
        }
    }

    let git_host = match GitHostService::from_url(&target_remote.url) {
        Ok(host) => host,
        Err(GitHostError::UnsupportedProvider) => {
            return Ok(AutoPrOutcome::Skipped {
                reason: "unsupported git hosting provider".to_string(),
            });
        }
        Err(GitHostError::CliNotInstalled { provider }) => {
            return Ok(AutoPrOutcome::Skipped {
                reason: format!("{provider} CLI is not installed"),
            });
        }
        Err(e) => return Err(e.into()),
    };

    let pr_request = CreatePrRequest {
        title: title.to_string(),
        body: body.map(str::to_string),
        head_branch: workspace.branch.clone(),
        base_branch: base_branch.clone(),
        draft: None,
        head_repo_url: Some(push_remote.url.clone()),
    };

    let pr_info = match git_host
        .create_pr(&repo.path, &target_remote.url, &pr_request)
        .await
    {
        Ok(pr_info) => pr_info,
        Err(GitHostError::CliNotInstalled { provider }) => {
            return Ok(AutoPrOutcome::Skipped {
                reason: format!("{provider} CLI is not installed"),
            });
        }
        Err(GitHostError::AuthFailed(_)) => {
            return Ok(AutoPrOutcome::Skipped {
                reason: "git host CLI is not logged in".to_string(),
            });
        }
        Err(GitHostError::UnsupportedProvider) => {
            return Ok(AutoPrOutcome::Skipped {
                reason: "unsupported git hosting provider".to_string(),
            });
        }
        Err(e) => return Err(e.into()),
    };

    PullRequest::create_for_workspace(
        pool,
        workspace.id,
        workspace_repo.repo_id,
        &base_branch,
        pr_info.number,
        &pr_info.url,
    )
    .await?;

    Ok(AutoPrOutcome::Created { url: pr_info.url })
}
