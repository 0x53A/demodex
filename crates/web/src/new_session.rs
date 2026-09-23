//! Session creation is independent of server administration and the open conversation.
use super::*;

impl App {
    pub(super) fn new_session_targets(&self) -> Vec<Value> {
        self.saved
            .fields
            .get("new_targets")
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_default()
    }

    pub(super) fn new_session_view(&self, ctx: &Context<Self>) -> Html {
        if !self.show_new_session {
            return Html::default();
        }
        let chosen = self.new_session_targets();
        let ssh = chosen
            .iter()
            .any(|target| text(target, "id").starts_with("ssh-"));
        let ready = !chosen.is_empty()
            && chosen.iter().all(|target| {
                text(target, "cwd").starts_with('/')
                    && self
                        .targets
                        .iter()
                        .any(|known| known["id"] == target["id"] && known["available"] == true)
            });
        let sandbox = self.saved.field("new_sandbox");
        let payload = json!({"name":self.saved.field("new_session_name"),"targets":chosen,"sandbox":nonempty(sandbox.clone())}).to_string();
        html! {<crate::modal::Modal title="New Session" onclose={ctx.link().callback(|_|Msg::NewSession(false))}>
            {self.modal_error(ctx)}
            {if self.runtime["running"]!=true || self.runtime["account"].is_null(){html!{<p class="muted">{"Start the Codex runtime and sign in from Server Settings before creating a session."}</p>}}else{Html::default()}}
            <form class="setup new-session-form" onsubmit={ctx.link().callback(move |event:SubmitEvent|{event.prevent_default();Msg::Run(Operation::CreateSession{input:payload.clone()})})}>
                <fieldset disabled={self.busy||!self.connected}>
                    {self.field(ctx,"new_session_name","Session name","What are you working on?")}
                    <h3>{"Execution targets"}</h3><p class="muted">{"Choose where this session can work. The first target is primary."}</p>
                    {for self.targets.iter().map(|target|{
                        let id=text(target,"id");
                        let vm=self.environments.iter().find(|env|env["id"]==target["environment_id"]);
                        let enabled=chosen.iter().any(|value|text(value,"id")==id);
                        let mut next=chosen.clone();
                        if enabled { next.retain(|value|text(value,"id")!=id); } else { next.push(json!({"id":id,"cwd":target["cwd"]})); }
                        let next=json!(next).to_string();
                        html!{<div class="creation-target"><label class="checkbox"><input type="checkbox" checked={enabled} onchange={ctx.link().callback(move |_|Msg::Field("new_targets".into(),next.clone()))}/>{format!("{} · {}",text(target,"name"),text(target,"kind"))}</label>
                            {if target["available"]!=true{html!{<span class="muted">{vm.map(|env|text(env,"status")).unwrap_or("Unavailable")}</span>}}else{Html::default()}}
                            {if let Some(error)=vm.and_then(|env|env["error"].as_str()){html!{<p class="error">{error}</p>}}else{Html::default()}}
                            {if target["kind"]=="vm" && target["available"]!=true && vm.is_some_and(|env|matches!(text(env,"status"),"stopped"|"error"|"created")){self.button(ctx,"Start VM",Operation::StartEnvironment{id:text(target,"environment_id").into()})}else{Html::default()}}
                        </div>}
                    })}
                    {if self.targets.is_empty(){html!{<p>{"No executors registered. Add an SSH executor in Server Settings or create a VM below."}</p>}}else{Html::default()}}
                    {for chosen.iter().enumerate().map(|(index,target)|{
                        let name=self.targets.iter().find(|known|known["id"]==target["id"]).map(|known|text(known,"name")).unwrap_or("Unavailable target");
                        let edited=chosen.clone();let mut removed=chosen.clone();removed.remove(index);let removed=json!(removed).to_string();
                        let mut primary=chosen.clone();let target_entry=primary.remove(index);primary.insert(0,target_entry);let primary=json!(primary).to_string();
                        html!{<div class="target-directory"><label>{format!("{}{} working directory",name,if index==0{" (primary)"}else{""})}<input value={text(target,"cwd").to_owned()} oninput={ctx.link().callback(move |e:InputEvent|{let mut next=edited.clone();next[index]["cwd"]=json!(input(e));Msg::Field("new_targets".into(),json!(next).to_string())})}/></label>
                            {if index>0{html!{<button type="button" onclick={ctx.link().callback(move |_|Msg::Field("new_targets".into(),primary.clone()))}>{"Make primary"}</button>}}else{Html::default()}}
                            <button type="button" onclick={ctx.link().callback(move |_|Msg::Field("new_targets".into(),removed.clone()))}>{"Remove"}</button>
                        </div>}
                    })}
                    {self.sandbox(ctx,"new_sandbox","Sandbox")}
                    {if ssh{html!{<p class="muted">{"SSH executes with the remote account’s authority. Select danger-full-access to continue."}</p>}}else{Html::default()}}
                    <button class="primary" type="submit" disabled={!ready||self.runtime["running"]!=true||self.runtime["account"].is_null()||self.saved.field("new_session_name").trim().is_empty()||(ssh&&sandbox!="danger-full-access")}>{"Create session"}</button>
                </fieldset>
            </form>
            <details class="create-vm"><summary>{"Create a VM"}</summary>{self.create_vm_form(ctx)}</details>
            {if text(&self.runtime,"mode")=="host"{html!{<details class="resume-session"><summary>{"Resume a saved session"}</summary>{self.resume_session_form(ctx)}</details>}}else{Html::default()}}
        </crate::modal::Modal>}
    }
    fn create_vm_form(&self, ctx: &Context<Self>) -> Html {
        let payload=json!({"name":self.saved.field("environment_name"),"memory_mib":self.saved.field("memory").parse::<u32>().unwrap_or(4096),"cpus":self.saved.field("cpus").parse::<u16>().unwrap_or(2),"internet":self.saved.field("internet")=="true"}).to_string();
        html! {<form class="setup" onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Run(Operation::CreateEnvironment{input:payload.clone()})})}><h2>{"Create a work VM"}</h2>{self.field(ctx,"environment_name","Environment name","Scratch workspace")}<div class="resource-fields"><label>{"Memory (MiB)"}<input type="number" min="512" max="65536" step="1" placeholder="4096" value={self.saved.field("memory")} oninput={ctx.link().callback(|e|Msg::Field("memory".into(),input(e)))}/></label><label>{"CPUs"}<input type="number" min="1" max="32" step="1" placeholder="2" value={self.saved.field("cpus")} oninput={ctx.link().callback(|e|Msg::Field("cpus".into(),input(e)))}/></label></div><label class="checkbox"><input type="checkbox" checked={self.saved.field("internet")=="true"} onchange={ctx.link().callback(|e:Event|Msg::Field("internet".into(),e.target_unchecked_into::<HtmlInputElement>().checked().to_string()))}/>{"Internet and LAN access"}</label><button disabled={self.busy||!self.connected}>{"Create and start"}</button></form>}
    }
    fn resume_session_form(&self, ctx: &Context<Self>) -> Html {
        let payload=json!({"name":self.saved.field("session_name"),"thread_id":nonempty(self.saved.field("thread_id")),"cwd":nonempty(self.saved.field("cwd")),"sandbox":nonempty(self.saved.field("sandbox"))}).to_string();
        html! {<>                <form class="setup" onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Run(Operation::HostSession{input:payload.clone()})})}><h3>{"Resume a saved host session"}</h3><p>{"Tools run with the service user's file and hardware access."}</p><p class="muted">{format!("Default working directory: {}",text(&self.runtime,"workspace"))}</p>
                            {self.field(ctx,"session_name","Resumed session name","Hardware workspace")}{self.field(ctx,"thread_id","Existing Codex thread ID","Resume a saved Codex session")}{self.field(ctx,"cwd","Working directory (optional)","Keep default / saved directory")}{self.sandbox(ctx,"sandbox","Resume sandbox")}<p class="muted">{"Exit an external CLI session before attaching its saved thread here."}</p><button class="primary" disabled={self.busy||!self.connected||self.runtime["account"].is_null()||self.saved.field("session_name").trim().is_empty()||self.saved.field("thread_id").trim().is_empty()}>{"Resume host session"}</button>
                        </form>
                        <section class="saved-threads"><h2>{"Saved Codex sessions"}</h2>{self.field(ctx,"search","Search saved sessions","Title contains…")}<button disabled={!self.connected||self.busy} onclick={ctx.link().callback(|_|Msg::LoadSaved(false))}>{"Find saved sessions"}</button>{for self.saved_threads.iter().map(|thread|{let t=thread.clone();let name=thread["name"].as_str().filter(|s|!s.is_empty()).unwrap_or_else(||text(thread,"preview"));html!{<button class="session" onclick={ctx.link().callback(move |_|Msg::ChooseThread(t.clone()))}><strong>{name}</strong><span>{text(thread,"cwd")}</span><small>{text(thread,"id")}</small></button>}})}{if self.cursor.is_some(){html!{<button onclick={ctx.link().callback(|_|Msg::LoadSaved(true))}>{"Load more"}</button>}}else{Html::default()}}</section>
        </>}
    }
}
