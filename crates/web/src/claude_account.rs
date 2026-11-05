use crate::client::Client;
use demodex_protocol::Operation;
use serde_json::Value;
use std::rc::Rc;
use web_sys::HtmlInputElement;
use yew::prelude::*;
#[derive(Properties)]
pub struct Props {
    pub client: Rc<Client>,
}
impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.client, &other.client)
    }
}
pub struct ClaudeAccount {
    status: Value,
    login: Value,
    code: String,
    busy: bool,
    error: Option<String>,
}
pub enum Msg {
    Load,
    Begin,
    Complete,
    Disconnect,
    Restart,
    Refresh,
    Code(String),
    Failed(String, Value),
    Done(bool, Result<Value, String>),
}
impl Component for ClaudeAccount {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        ctx.link().send_message(Msg::Load);
        Self {
            status: Value::Null,
            login: Value::Null,
            code: String::new(),
            busy: false,
            error: None,
        }
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        let op = match msg {
            Msg::Code(s) => {
                self.code = s;
                return true;
            }
            Msg::Failed(error, status) => {
                self.busy = false;
                self.error = Some(error);
                if !status.is_null() {
                    self.status = status;
                    if self.status["connected"] == true {
                        self.login = Value::Null;
                    }
                }
                return true;
            }
            Msg::Done(login, result) => {
                self.busy = false;
                match result {
                    Ok(v) => {
                        if login {
                            self.login = v;
                        } else {
                            self.status = v;
                            self.login = Value::Null;
                        }
                    }
                    Err(e) => {
                        self.error = Some(e);
                    }
                }
                return true;
            }
            Msg::Load => Operation::ClaudeAccount,
            Msg::Begin => Operation::BeginClaudeLogin,
            Msg::Complete => {
                let code = std::mem::take(&mut self.code);
                Operation::CompleteClaudeLogin {
                    login_id: self.login["id"].as_str().unwrap_or("").into(),
                    code,
                }
            }
            Msg::Disconnect => Operation::DisconnectClaudeAccount,
            Msg::Restart => Operation::RestartClaudeRuntime,
            Msg::Refresh => Operation::RefreshClaudeModels,
        };
        if self.busy {
            return false;
        }
        self.busy = true;
        self.error = None;
        let client = ctx.props().client.clone();
        let login = matches!(op, Operation::BeginClaudeLogin);
        ctx.link().send_future(async move {
            let result = if op.is_mutation() {
                client.call(op, uuid::Uuid::new_v4().to_string()).await
            } else {
                client.read(op).await
            };
            if let Err(error) = &result {
                let status = client
                    .read(Operation::ClaudeAccount)
                    .await
                    .unwrap_or(Value::Null);
                return Msg::Failed(error.to_string(), status);
            }
            Msg::Done(
                login,
                match result {
                    Ok(_) if !login => client
                        .read(Operation::ClaudeAccount)
                        .await
                        .map_err(|e| e.to_string()),
                    other => other.map_err(|e| e.to_string()),
                },
            )
        });
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let connected = self.status["connected"] == true;
        html! {<crate::ui::Group class="runtime-panel account-status" title="Claude account">
            {if connected{html!{<><p>{"Connected"}</p><button disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Refresh)}>{"Refresh models"}</button><button disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Disconnect)}>{"Disconnect"}</button></>}}
            else{html!{<button disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Begin)}>{"Sign in with Claude"}</button>}}}
            {if self.status["restart_required"]==true{html!{<><p>{"Restart the Codex runtime to enable Claude models and mixed-model subagents. Active sessions must finish first."}</p><button disabled={self.busy} onclick={ctx.link().callback(|_|Msg::Restart)}>{"Restart Claude runtime"}</button></>}}else{Html::default()}}
            {if let Some(url)=self.login["url"].as_str(){html!{<>
                <p><a href={url.to_owned()} target="_blank" rel="noopener noreferrer">{"Continue sign-in in your browser"}</a></p>
                <label>{"Authorization code"}<input type="password" autocomplete="off" value={self.code.clone()} oninput={ctx.link().callback(|e:InputEvent|Msg::Code(e.target_unchecked_into::<HtmlInputElement>().value()))}/></label>
                <button disabled={self.busy||self.code.trim().is_empty()} onclick={ctx.link().callback(|_|Msg::Complete)}>{"Connect account"}</button>
            </>}}else{Html::default()}}
            {if let Some(e)=self.error.as_deref().or_else(||self.status["error"].as_str()){html!{<p role="alert">{e}</p>}}else{Html::default()}}
        </crate::ui::Group>}
    }
}
