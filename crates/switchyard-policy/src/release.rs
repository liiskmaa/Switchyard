//! Commands that publish work beyond the workspace (ADR 0042): git commits
//! and pushes, package publishing, deployments and releases. Auto mode never
//! allows these by itself; they always ask the operator.
//!
//! This is a deliberately broad text scan, not a shell parser: every word of
//! the command line is looked at, including commands nested in
//! `zsh -lc "…"`, `bash -c`, `&&`, pipes and subshells. A false positive
//! only means one more question; a miss would mean an unapproved release.

/// Words that separate commands: `;`, `&&`, `|`, subshells and the like.
const SEPARATORS: &[char] = &[';', '&', '|', '(', ')', '`', '$', '<', '>', '{', '}', '\n', '\r', '\t', ' '];

/// The command's words, lowercased, as written (`raw`) and as program names
/// (`/usr/bin/git` → `git`).
fn words(command: &str) -> (Vec<String>, Vec<String>) {
    let cleaned: String = command.chars().filter(|c| !matches!(c, '"' | '\'' | '\\')).collect();
    let raw: Vec<String> = cleaned
        .split(|c: char| SEPARATORS.contains(&c))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let base =
        raw.iter().map(|w| w.trim_end_matches('/').rsplit('/').next().unwrap_or(w).to_owned()).collect();
    (raw, base)
}

/// The first word after `i` that is not an option (and not the value of one
/// of `takes_value`), with its index.
fn subcommand<'a>(w: &'a [String], i: usize, takes_value: &[&str]) -> Option<(usize, &'a str)> {
    let mut j = i + 1;
    while j < w.len() {
        let t = w[j].as_str();
        if t.starts_with('-') {
            j += if takes_value.contains(&t) { 2 } else { 1 };
            continue;
        }
        return Some((j, t));
    }
    None
}

fn is_release_name(name: &str) -> bool {
    ["deploy", "release", "publish", "ship"].iter().any(|p| name.starts_with(p))
}

/// What the command would release, if it would: e.g. `git push`, `npm publish`.
pub fn release_action(command: &str) -> Option<String> {
    let (raw, w) = words(command);
    for i in 0..w.len() {
        let tool = w[i].as_str();
        let sub = |takes: &[&str]| subcommand(&w, i, takes).map(|(_, s)| s.to_owned());
        let hit = match tool {
            "git" => match sub(&["-c", "-C", "--git-dir", "--work-tree", "--namespace"]).as_deref() {
                Some(s @ ("commit" | "push")) => Some(format!("git {s}")),
                _ => None,
            },
            "npm" | "pnpm" | "yarn" | "bun" => {
                match subcommand(&w, i, &["--prefix", "-C", "--cwd", "--filter", "-w", "--workspace"]) {
                    Some((_, "publish")) => Some(format!("{tool} publish")),
                    Some((j, "run" | "run-script")) => subcommand(&w, j, &[])
                        .filter(|(_, s)| is_release_name(s))
                        .map(|(_, s)| format!("{tool} run {s}")),
                    Some((_, s)) if tool != "npm" && is_release_name(s) => Some(format!("{tool} {s}")),
                    _ => None,
                }
            }
            "make" | "just" | "task" | "rake" => subcommand(&w, i, &["-c", "-f", "-C"])
                .filter(|(_, s)| is_release_name(s))
                .map(|(_, s)| format!("{tool} {s}")),
            "gradle" | "gradlew" | "mvn" | "mvnw" => w[i + 1..]
                .iter()
                .find(|t| {
                    !t.starts_with('-')
                        && (t.contains("publish") || t.contains("deploy") || t.contains("release"))
                })
                .map(|t| format!("{tool} {t}")),
            "cargo" | "poetry" | "gem" | "twine" | "dotnet" | "swift" => match sub(&[]).as_deref() {
                Some(s @ ("publish" | "push" | "upload")) => Some(format!("{tool} {s}")),
                Some("nuget") => Some(format!("{tool} nuget")),
                _ => None,
            },
            "docker" | "podman" | "buildah" => {
                match sub(&["--context", "-c", "--config", "-h", "--host"]).as_deref() {
                    Some("push") => Some(format!("{tool} push")),
                    Some("buildx") if w[i..].iter().any(|t| t == "--push") => {
                        Some(format!("{tool} buildx --push"))
                    }
                    _ => None,
                }
            }
            "kubectl" | "oc" => {
                match sub(&["-n", "--namespace", "--context", "--kubeconfig", "-f"]).as_deref() {
                    Some(
                        s @ ("apply" | "create" | "delete" | "replace" | "patch" | "rollout" | "scale"
                        | "set" | "edit" | "annotate" | "label" | "expose"),
                    ) => Some(format!("{tool} {s}")),
                    _ => None,
                }
            }
            "helm" => match sub(&["-n", "--namespace", "--kube-context"]).as_deref() {
                Some(s @ ("install" | "upgrade" | "uninstall" | "rollback" | "delete")) => {
                    Some(format!("helm {s}"))
                }
                _ => None,
            },
            "terraform" | "tofu" | "terragrunt" => match sub(&["-chdir"]).as_deref() {
                Some(s @ ("apply" | "destroy" | "import" | "state")) => Some(format!("{tool} {s}")),
                _ => None,
            },
            "pulumi" => match sub(&["-s", "--stack", "-C", "--cwd"]).as_deref() {
                Some(s @ ("up" | "destroy" | "update")) => Some(format!("pulumi {s}")),
                _ => None,
            },
            "vercel" => {
                match sub(&["--cwd", "-A", "--local-config", "-S", "--scope", "-t", "--token"]).as_deref() {
                    Some(
                        "dev" | "env" | "login" | "logout" | "whoami" | "ls" | "list" | "logs" | "inspect"
                        | "pull" | "link" | "help",
                    ) => None,
                    _ => Some("vercel deploy".to_owned()),
                }
            }
            "gh" => match subcommand(&w, i, &["-R", "--repo"]) {
                Some((j, "release")) => subcommand(&w, j, &[])
                    .filter(|(_, s)| matches!(*s, "create" | "upload" | "edit" | "delete"))
                    .map(|(_, s)| format!("gh release {s}")),
                Some((j, "pr")) => {
                    subcommand(&w, j, &[]).filter(|(_, s)| *s == "merge").map(|_| "gh pr merge".to_owned())
                }
                Some((j, "workflow")) => {
                    subcommand(&w, j, &[]).filter(|(_, s)| *s == "run").map(|_| "gh workflow run".to_owned())
                }
                _ => None,
            },
            "glab" => w[i + 1..]
                .iter()
                .any(|t| t == "release" || t == "merge")
                .then(|| "glab release/merge".to_owned()),
            // Cloud and hosting CLIs: any `deploy`, `publish`, `release`, `up` or `push` subcommand.
            "netlify" | "fly" | "flyctl" | "firebase" | "wrangler" | "serverless" | "sls" | "eb"
            | "railway" | "render" | "amplify" | "cdk" | "sam" | "heroku" | "gcloud" | "az" | "aws"
            | "doctl" | "copilot" | "expo" | "eas" | "fastlane" | "supabase" | "convex" => w[i + 1..]
                .iter()
                .take(6)
                .find(|t| {
                    matches!(t.as_str(), "deploy" | "publish" | "release" | "up" | "push" | "submit")
                        || t.starts_with("deploy")
                        || t.starts_with("releases:")
                        || t.starts_with("container:release")
                })
                .map(|t| format!("{tool} {t}")),
            "ansible-playbook" | "capistrano" | "cap" => Some(tool.to_owned()),
            // Copying to or running on another machine.
            "scp" | "rsync" if raw[i + 1..].iter().any(|t| remote_target(t)) => {
                Some(format!("{tool} to a remote host"))
            }
            "ssh" => Some("ssh".to_owned()),
            _ => None,
        };
        if hit.is_some() {
            return hit;
        }
    }
    None
}

/// `host:path` or `user@host:path` (not a URL, not a Windows drive).
fn remote_target(t: &str) -> bool {
    match t.split_once(':') {
        Some((host, _)) => {
            host.len() > 1
                && !host.contains("//")
                && host.chars().all(|c| c.is_ascii_alphanumeric() || "@.-_".contains(c))
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::release_action;

    #[test]
    fn git_commits_and_pushes_are_releases_wherever_they_appear() {
        for (cmd, want) in [
            ("git commit -m 'x'", "git commit"),
            ("git -C repo push origin main", "git push"),
            ("/usr/bin/git push --tags", "git push"),
            ("/bin/zsh -lc \"cd app && git add -A && git commit -m wip\"", "git commit"),
            ("bash -c 'npm test; git push'", "git push"),
            ("git -c user.name=x commit --amend", "git commit"),
            ("FOO=1 git push", "git push"),
        ] {
            assert_eq!(release_action(cmd).as_deref(), Some(want), "{cmd}");
        }
    }

    #[test]
    fn deployments_and_publishing_are_releases() {
        for (cmd, want) in [
            ("npm publish", "npm publish"),
            ("pnpm run deploy:prod", "pnpm run deploy:prod"),
            ("yarn release", "yarn release"),
            ("npm run release", "npm run release"),
            ("make deploy", "make deploy"),
            ("cargo publish --dry-run", "cargo publish"),
            ("docker push registry/app:1", "docker push"),
            ("kubectl -n prod apply -f k8s.yaml", "kubectl apply"),
            ("helm upgrade app ./chart", "helm upgrade"),
            ("terraform apply -auto-approve", "terraform apply"),
            ("vercel --prod", "vercel deploy"),
            ("npx vercel", "vercel deploy"),
            ("gh release create v1.2", "gh release create"),
            ("gh pr merge 12 --squash", "gh pr merge"),
            ("fly deploy", "fly deploy"),
            ("firebase deploy --only hosting", "firebase deploy"),
            ("wrangler deploy", "wrangler deploy"),
            ("gcloud run deploy api --source .", "gcloud deploy"),
            ("rsync -avz dist/ me@server:/var/www/", "rsync to a remote host"),
            ("ssh deploy@server 'systemctl restart app'", "ssh"),
            ("./gradlew publishToMavenCentral", "gradlew publishtomavencentral"),
        ] {
            assert_eq!(release_action(cmd).as_deref(), Some(want), "{cmd}");
        }
    }

    #[test]
    fn everyday_commands_are_not_releases() {
        for cmd in [
            "git status",
            "git diff HEAD~1",
            "git log --oneline",
            "git add -A",
            "git stash",
            "npm test",
            "npm run build",
            "pnpm install",
            "yarn test",
            "make test",
            "cargo test --workspace",
            "docker build -t app .",
            "kubectl get pods",
            "helm template ./chart",
            "terraform plan",
            "vercel dev",
            "gh pr view 12",
            "rsync -a src/ build/",
            "ls -la && cat README.md | grep deploy",
            "/bin/zsh -lc \"rg -n 'mailbox' src\"",
        ] {
            assert_eq!(release_action(cmd), None, "{cmd}");
        }
    }
}
