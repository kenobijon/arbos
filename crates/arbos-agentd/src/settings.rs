//! Project settings (`Project.models`, name, icon, instructions, spend cap) kept in sync three ways:
//! relay registry (REST) ⇄ project doc `settings` map (CRDT, what clients read and may write) ⇄ the
//! live pi session (`set_model` / `set_thinking_level`). The box is the only writer of the doc's
//! `chats` index and `status`; clients may write `settings.agentModel` / `agentEffort` (SetModel).

use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::sync::RwLock;

use arbos_core::rpc::Command as RpcCommand;
use arbos_core::wire::PROJECT_DOC_CHAT_ID;

use crate::agent::Agent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub id: String,
    pub effort: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Models {
    pub talking: Slot,
    pub subagent: Slot,
    pub decision: Slot,
}

impl Models {
    pub fn from_json(v: &Value, fallback: &Models) -> Models {
        let slot = |k: &str, f: &Slot| Slot {
            id: v
                .pointer(&format!("/{k}/id"))
                .and_then(Value::as_str)
                .unwrap_or(&f.id)
                .to_owned(),
            effort: v
                .pointer(&format!("/{k}/effort"))
                .and_then(Value::as_str)
                .unwrap_or(&f.effort)
                .to_owned(),
        };
        Models {
            talking: slot("talking_model", &fallback.talking),
            subagent: slot("subagent_model", &fallback.subagent),
            decision: slot("decision_model", &fallback.decision),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "talking_model": { "id": self.talking.id, "effort": self.talking.effort },
            "subagent_model": { "id": self.subagent.id, "effort": self.subagent.effort },
            "decision_model": { "id": self.decision.id, "effort": self.decision.effort },
        })
    }
}

/// UI effort words → pi thinking levels (`off minimal low medium high xhigh max`).
pub fn effort_to_thinking(effort: &str) -> &'static str {
    match effort.to_ascii_lowercase().as_str() {
        "off" | "none" => "off",
        "minimal" => "minimal",
        "low" => "low",
        "medium" | "med" => "medium",
        "high" => "high",
        "xhigh" => "xhigh",
        "max" => "max",
        _ => "off",
    }
}

#[derive(Debug, Default, Clone)]
pub struct SettingsState {
    pub name: String,
    pub icon: String,
    pub color: String,
    pub instructions: String,
    pub spend_cap_usd: f64,
    /// Last `settings` JSON the box wrote into the project doc; used to tell client writes from echoes.
    pub last_doc_write: Option<Value>,
    pub last_chats_write: Option<Value>,
}

pub struct Settings {
    pub models: RwLock<Models>,
    pub state: RwLock<SettingsState>,
}

impl Settings {
    pub fn new(cfg: &crate::config::Config) -> Self {
        let models = Models {
            talking: Slot {
                id: cfg.talking_model.clone(),
                effort: "off".into(),
            },
            subagent: Slot {
                id: cfg.subagent_model.clone(),
                effort: "medium".into(),
            },
            decision: Slot {
                id: cfg.decision_model.clone(),
                effort: "off".into(),
            },
        };
        Self {
            models: RwLock::new(models),
            state: RwLock::new(SettingsState {
                name: cfg.project_name.clone(),
                instructions: cfg.instructions.clone(),
                spend_cap_usd: 25.0,
                ..Default::default()
            }),
        }
    }
}

impl Agent {
    /// The relay registry view arrived (boot hello or the 3 s poll): apply models and mirror into the doc.
    pub async fn settings_from_relay(
        self: &Arc<Self>,
        project: &Value,
        chats: &[Value],
    ) -> Result<()> {
        let current = self.settings.models.read().await.clone();
        let incoming = project
            .get("models")
            .map(|m| Models::from_json(m, &current))
            .unwrap_or_else(|| current.clone());
        if incoming != current {
            self.apply_models(&incoming).await?;
        }
        {
            let mut st = self.settings.state.write().await;
            if let Some(n) = project.get("name").and_then(Value::as_str) {
                st.name = n.to_owned();
            }
            if let Some(i) = project.get("icon").and_then(Value::as_str) {
                st.icon = i.to_owned();
            }
            if let Some(c) = project.get("color").and_then(Value::as_str) {
                st.color = c.to_owned();
            }
            if let Some(i) = project.get("instructions").and_then(Value::as_str) {
                st.instructions = i.to_owned();
            }
            if let Some(c) = project.get("spend_cap_usd").and_then(Value::as_f64) {
                st.spend_cap_usd = c;
            }
        }
        self.write_settings_doc().await;
        self.write_chats_index(chats).await;
        Ok(())
    }

    /// Switch the live pi session's model / thinking level and remember the new slots.
    pub async fn apply_models(self: &Arc<Self>, m: &Models) -> Result<()> {
        let prev = self.settings.models.read().await.clone();
        if let Ok(pi) = self.pi_child().await {
            if prev.talking.id != m.talking.id {
                let r = pi
                    .call(RpcCommand::SetModel {
                        id: pi.next_id("sm"),
                        provider: self.cfg.pi_provider(),
                        model_id: m.talking.id.clone(),
                    })
                    .await?;
                if !r.success {
                    tracing::warn!(model = %m.talking.id, error = ?r.error, "set_model refused");
                }
            }
            if prev.talking.effort != m.talking.effort {
                let _ = pi
                    .call(RpcCommand::SetThinkingLevel {
                        id: pi.next_id("tl"),
                        level: effort_to_thinking(&m.talking.effort).into(),
                    })
                    .await;
            }
        }
        *self.settings.models.write().await = m.clone();
        tracing::info!(talking = %m.talking.id, effort = %m.talking.effort, subagent = %m.subagent.id, "models applied");
        Ok(())
    }

    /// Mirror the registry settings into the project doc (`settings` map, crdt-sync §6.3).
    pub async fn write_settings_doc(self: &Arc<Self>) {
        let m = self.settings.models.read().await.clone();
        let st = self.settings.state.read().await.clone();
        let v = json!({
            "name": st.name, "icon": st.icon, "color": st.color, "instructions": st.instructions,
            "agentModel": m.talking.id, "agentEffort": m.talking.effort,
            "defaultSubagentModel": m.subagent.id, "defaultSubagentEffort": m.subagent.effort,
            "decisionModel": m.decision.id, "spendCapUsd": st.spend_cap_usd,
        });
        if self.settings.state.read().await.last_doc_write.as_ref() == Some(&v) {
            return;
        }
        let Some(h) = self.sync.host(PROJECT_DOC_CHAT_ID).await else {
            return;
        };
        {
            let h = h.lock().await;
            let settings = h.doc.doc.get_map("settings");
            if let Some(o) = v.as_object() {
                for (k, val) in o {
                    let lv: loro::LoroValue =
                        serde_json::from_value(val.clone()).unwrap_or(loro::LoroValue::Null);
                    settings.insert(k, lv).ok();
                }
            }
            h.doc.doc.commit();
        }
        self.settings.state.write().await.last_doc_write = Some(v);
    }

    /// `chats` index in the project doc (box is the only writer); rewritten only when it changed.
    pub async fn write_chats_index(self: &Arc<Self>, chats: &[Value]) {
        let v = Value::Array(chats.to_vec());
        if self.settings.state.read().await.last_chats_write.as_ref() == Some(&v) {
            return;
        }
        let Some(h) = self.sync.host(PROJECT_DOC_CHAT_ID).await else {
            return;
        };
        {
            let h = h.lock().await;
            let list = h.doc.doc.get_movable_list("chats");
            let n = list.len();
            if n > 0 {
                list.delete(0, n).ok();
            }
            for c in chats {
                let entry = json!({ "id": c.get("id"), "title": c.get("title"), "isPinned": c.get("is_pinned"), "isSystem": c.get("is_system").and_then(Value::as_bool).unwrap_or(false), "lastMessageAt": c.get("last_message_at"), "position": c.get("position") });
                let lv: loro::LoroValue =
                    serde_json::from_value(entry).unwrap_or(loro::LoroValue::Null);
                list.push(lv).ok();
            }
            h.doc.doc.commit();
        }
        self.settings.state.write().await.last_chats_write = Some(v);
    }

    /// A client wrote into the project doc: pick up `settings.agentModel` / `agentEffort` (SetModel)
    /// and push it to the registry so every peer converges.
    pub async fn settings_from_doc(self: &Arc<Self>) {
        let Some(h) = self.sync.host(PROJECT_DOC_CHAT_ID).await else {
            return;
        };
        let settings: Value = {
            let h = h.lock().await;
            serde_json::to_value(h.doc.doc.get_map("settings").get_deep_value())
                .unwrap_or(Value::Null)
        };
        let current = self.settings.models.read().await.clone();
        let model = settings
            .get("agentModel")
            .and_then(Value::as_str)
            .unwrap_or(&current.talking.id)
            .to_owned();
        let effort = settings
            .get("agentEffort")
            .and_then(Value::as_str)
            .unwrap_or(&current.talking.effort)
            .to_owned();
        let sub_model = settings
            .get("defaultSubagentModel")
            .and_then(Value::as_str)
            .unwrap_or(&current.subagent.id)
            .to_owned();
        let sub_effort = settings
            .get("defaultSubagentEffort")
            .and_then(Value::as_str)
            .unwrap_or(&current.subagent.effort)
            .to_owned();
        let wanted = Models {
            talking: Slot { id: model, effort },
            subagent: Slot {
                id: sub_model,
                effort: sub_effort,
            },
            decision: current.decision.clone(),
        };
        if wanted == current {
            return;
        }
        tracing::info!(?wanted, "settings change from a client in the project doc");
        if let Err(e) = self.apply_models(&wanted).await {
            tracing::warn!(error = %e, "apply models from doc");
        }
        let _ = self
            .http_client()
            .patch(format!(
                "{}/projects/{}",
                self.cfg.relay_url, self.cfg.project_id
            ))
            .bearer_auth(&self.cfg.box_token)
            .json(&json!({ "models": wanted.to_json() }))
            .send()
            .await;
        self.write_settings_doc().await;
    }
}
