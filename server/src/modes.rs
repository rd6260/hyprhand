//! Modes / scenes — run composite Hyprland layouts defined in the config.

use serde_json::Value;
use tokio::time::{sleep, Duration};

use crate::config::{Mode, Step};
use crate::hypr;

pub async fn run(mode: &Mode) -> Value {
    let mut done: Vec<String> = vec![];

    for step in &mode.steps {
        match step {
            Step::Launch { cmd, workspace, r#match, once } => {
                let ws = workspace.unwrap_or(1);
                let name = cmd.split_whitespace().next().unwrap_or(cmd)
                    .rsplit('/').next().unwrap_or(cmd);

                let existing = if let Some(cls) = r#match {
                    hypr::clients_json().await
                        .into_iter()
                        .filter(|c| c["class"].as_str() == Some(cls.as_str()))
                        .collect::<Vec<_>>()
                } else {
                    vec![]
                };

                if *once && !existing.is_empty() {
                    let c = &existing[0];
                    let c_ws = c["workspace"]["id"].as_i64().unwrap_or(0) as i32;
                    if c_ws != ws {
                        let addr = c["address"].as_str().unwrap_or("");
                        hypr::move_window_to_workspace_by_address(addr, ws).await;
                        done.push(format!("placed {name} → ws{ws}"));
                    } else {
                        done.push(format!("skip {name} (open)"));
                    }
                    continue;
                }

                let before_addrs: std::collections::HashSet<String> = existing
                    .iter()
                    .filter_map(|c| c["address"].as_str().map(|s| s.to_string()))
                    .collect();

                hypr::exec_on_workspace(cmd, ws).await;

                // If we have a match class, wait for the new window to appear.
                if let Some(cls) = r#match {
                    let mut note = String::new();
                    for _ in 0..40 {
                        sleep(Duration::from_millis(200)).await;
                        let clients = hypr::clients_json().await;
                        let new: Vec<_> = clients.iter()
                            .filter(|c| {
                                c["class"].as_str() == Some(cls.as_str())
                                    && !before_addrs.contains(c["address"].as_str().unwrap_or(""))
                            })
                            .collect();
                        if let Some(c) = new.first() {
                            let c_ws = c["workspace"]["id"].as_i64().unwrap_or(0) as i32;
                            let addr = c["address"].as_str().unwrap_or("");
                            if c_ws != ws {
                                hypr::move_window_to_workspace_by_address(addr, ws).await;
                                note = " (moved)".to_string();
                            }
                            break;
                        }
                    }
                    done.push(format!("launch ws{ws}: {name}{note}"));
                } else {
                    done.push(format!("launch ws{ws}: {name}"));
                }
            }

            Step::Close { r#match: cls, workspaces } => {
                let clients = hypr::clients_json().await;
                let mut closed = 0usize;
                for c in clients {
                    if c["class"].as_str() != Some(cls.as_str()) { continue; }
                    let c_ws = c["workspace"]["id"].as_i64().unwrap_or(0) as i32;
                    if !workspaces.is_empty() && !workspaces.contains(&c_ws) { continue; }
                    let addr = c["address"].as_str().unwrap_or("");
                    hypr::close_window(addr).await;
                    closed += 1;
                }
                done.push(format!("close {cls} ×{closed}"));
            }

            Step::Move { r#match: cls, workspace: ws } => {
                let clients = hypr::clients_json().await;
                for c in clients {
                    if c["class"].as_str() != Some(cls.as_str()) { continue; }
                    let addr = c["address"].as_str().unwrap_or("");
                    hypr::move_window_to_workspace_by_address(addr, *ws).await;
                }
                done.push(format!("move {cls} → ws{ws}"));
            }

            Step::Focus { workspace } => {
                hypr::focus_workspace(*workspace).await;
                done.push(format!("focus ws{workspace}"));
            }
        }

        sleep(Duration::from_millis(150)).await;
    }

    serde_json::json!({ "ok": true, "steps": done })
}
