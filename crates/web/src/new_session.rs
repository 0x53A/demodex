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
        let resume = self.saved.field("resume_session") == "true";
        let ssh = self.saved.field("new_ssh") == "true";
        let chosen = self.new_session_targets();
        let unrestricted = ssh || chosen
            .iter()
            .any(|target| matches!(text(target, "id").split('-').next(), Some("ssh" | "container")));
        let ready = chosen.iter().all(|target| {
                text(target, "cwd").starts_with('/')
                    && self
                        .targets
                        .iter()
                        .any(|known| known["id"] == target["id"] && known["available"] == true)
            });
        let sandbox = self.saved.field("new_sandbox");
        let payload = sandbox_choice(&sandbox).map(|sandbox| demodex_protocol::SelectedSession {
            name: self.saved.field("new_session_name"),
            targets: chosen
                .iter()
                .map(|target| demodex_protocol::Selection {
                    id: text(target, "id").into(),
                    cwd: text(target, "cwd").into(),
                })
                .collect(),
            sandbox,
        });
        let operation = if resume && self.saved.field("thread_id").trim().is_empty() {
            Err("Choose a saved session or enter its thread ID.".into())
        } else if resume {
            sandbox_choice(&self.saved.field("sandbox")).map(|sandbox|Operation::HostSession{input:demodex_protocol::HostSession {
                name:self.saved.field("session_name"), thread_id:nonempty(self.saved.field("thread_id")),
                cwd:nonempty(self.saved.field("cwd")),sandbox
            }})
        } else { payload.map(|input|Operation::CreateSession{input}) };
        html! {<><crate::modal::Modal title="New Session" compact=true onclose={ctx.link().callback(|_|Msg::NewSession(false))}>
            {self.modal_error(ctx)}
            {if self.runtime["running"]!=true || self.runtime["account"].is_null(){html!{<p class="muted">{"Start the Codex runtime and sign in from Server Settings before creating a session."}</p>}}else{Html::default()}}
            <form class="setup new-session-form" onsubmit={ctx.link().callback(move |event:SubmitEvent|{event.prevent_default();operation.clone().map_or_else(Msg::InvalidForm,Msg::Run)})}>
                <fieldset disabled={self.busy||!self.connected}>
                    <crate::ui::SectionTitle>{"Session"}</crate::ui::SectionTitle>
                    <label class="checkbox"><input type="checkbox" checked={resume} onchange={ctx.link().callback(|e:Event|Msg::Field("resume_session".into(),e.target_unchecked_into::<HtmlInputElement>().checked().to_string()))}/>{"Resume existing session"}</label>
                    {if resume {self.resume_settings(ctx)}else{html!{<>
                    {self.field(ctx,"new_session_name","Session name (optional)","Untitled session")}
                    <crate::ui::SectionTitle>{"Executors"}</crate::ui::SectionTitle>
                    {for self.targets.iter().filter(|target|target["owner"].is_null()).map(|target|{
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
                    <div class="target-add-actions">
                        <crate::ui::AddButton onclick={ctx.link().callback(|_|Msg::NewTargetSetup("vm".into()))}>{"+ New VM"}</crate::ui::AddButton>
                        <crate::ui::AddButton onclick={ctx.link().callback(|_|Msg::NewTargetSetup("container".into()))}>{"+ New container"}</crate::ui::AddButton>
                        <crate::ui::AddButton onclick={ctx.link().callback(|_|Msg::NewTargetSetup("ssh".into()))}>{"+ Add SSH"}</crate::ui::AddButton>
                    </div>
                    {if ssh{html!{<div class="staged-ssh"><span>{format!("SSH · {} · {}",self.saved.field("new_ssh_name"),self.saved.field("new_ssh_destination"))}</span><button type="button" onclick={ctx.link().callback(|_|Msg::Field("new_ssh".into(),"false".into()))}>{"Remove"}</button></div>}}else{Html::default()}}
                    {for chosen.iter().enumerate().map(|(index,target)|{
                        let name=self.targets.iter().find(|known|known["id"]==target["id"]).map(|known|text(known,"name")).unwrap_or("Unavailable target");
                        let edited=chosen.clone();let mut removed=chosen.clone();removed.remove(index);let removed=json!(removed).to_string();
                        let mut primary=chosen.clone();let target_entry=primary.remove(index);primary.insert(0,target_entry);let primary=json!(primary).to_string();
                        html!{<div class="target-directory"><crate::ui::FieldAction><label>{format!("{}{} working directory",name,if index==0{" (primary)"}else{""})}<input value={text(target,"cwd").to_owned()} oninput={ctx.link().callback(move |e:InputEvent|{let mut next=edited.clone();next[index]["cwd"]=json!(input(e));Msg::Field("new_targets".into(),json!(next).to_string())})}/></label>
                        <crate::directory_picker::DirectoryPicker key={format!("{}:{}",self.generation,text(target,"id"))} client={self.connected.then(||self.client.clone()).flatten()} target={text(target,"id").to_owned()} path={text(target,"cwd").to_owned()} onchoose={ctx.link().callback({let targets=chosen.clone();let id=text(target,"id").to_owned();move |path:String|{let mut next=targets.clone();if let Some(target)=next.iter_mut().find(|t|t["id"]==id){target["cwd"]=json!(path);}Msg::Field("new_targets".into(),json!(next).to_string())}})}/></crate::ui::FieldAction>
                            {if index>0{html!{<button type="button" onclick={ctx.link().callback(move |_|Msg::Field("new_targets".into(),primary.clone()))}>{"Make primary"}</button>}}else{Html::default()}}
                            <button type="button" onclick={ctx.link().callback(move |_|Msg::Field("new_targets".into(),removed.clone()))}>{"Remove"}</button>
                        </div>}
                    })}
                    <crate::ui::SectionTitle>{"Permissions"}</crate::ui::SectionTitle>
                    {self.sandbox(ctx,"new_sandbox","Sandbox")}
                    {if unrestricted && sandbox!="danger-full-access"{html!{<p class="muted control-warning" role="status">{"SSH and container executors require danger-full-access."}</p>}}else{Html::default()}}
                    </>}}}
                    <button class="primary" type="submit" disabled={self.runtime["running"]!=true||self.runtime["account"].is_null()||if resume{self.saved.field("thread_id").trim().is_empty()||text(&self.runtime,"mode")!="host"}else{!ready||(unrestricted&&sandbox!="danger-full-access")}}>{if resume{"Resume session"}else{"Create session"}}</button>
                </fieldset>
            </form>
        </crate::modal::Modal>
        {if self.show_saved_search{html!{<crate::modal::Modal title="Search sessions" compact=true onclose={ctx.link().callback(|_|Msg::SavedSearch(false))}>
            {self.modal_error(ctx)}
            <form class="saved-search" onsubmit={ctx.link().callback(|e:SubmitEvent|{e.prevent_default();Msg::LoadSaved(false)})}>
                <label for="saved-session-search">{"Search saved sessions"}</label><div class="search-input"><input id="saved-session-search" type="search" value={self.saved.field("search")} placeholder="Title or session ID…" oninput={ctx.link().callback(|e|Msg::Field("search".into(),input(e)))}/><button type="submit" disabled={!self.connected||self.busy}>{"🔍 Search"}</button></div>
            </form>
            {for self.saved_threads.iter().map(|thread|{let t=thread.clone();let name=thread["name"].as_str().filter(|s|!s.is_empty()).unwrap_or_else(||text(thread,"preview"));html!{
                <button class="session saved-thread" onclick={ctx.link().callback(move |_|Msg::ChooseThread(t.clone()))}><strong>{name}</strong><small>{text(thread,"cwd")}</small><small>{text(thread,"id")}</small></button>
            }})}
            {if self.saved_search_loading{html!{<p role="status">{"Searching saved sessions…"}</p>}}else{Html::default()}}
            {if self.cursor.is_some(){html!{<button disabled={self.saved_search_loading||!self.connected} onclick={ctx.link().callback(|_|Msg::LoadSaved(true))}>{"Load more"}</button>}}else{Html::default()}}
        </crate::modal::Modal>}}else{Html::default()}}
        </>}
    }
    pub(super) fn target_setup_view(&self, ctx: &Context<Self>) -> Html {
        html!{<>        {if !self.new_target_setup.is_empty(){html!{<crate::modal::Modal title={match self.new_target_setup.as_str(){"vm"=>"New VM","container"=>"New container","shared-ssh"=>"Add SSH executor","external"=>"Register external executor","session-ssh"=>"Add session SSH executor",_=>"Add SSH"}} compact=true onclose={ctx.link().callback(|_|Msg::NewTargetSetup(String::new()))}>
            {self.modal_error(ctx)}
            {match self.new_target_setup.as_str() {
                "vm"=>self.create_vm_form(ctx),
                "container"=>self.create_container_form(ctx),
                "shared-ssh"=>self.shared_ssh_form(ctx),
                "external"=>self.external_target_form(ctx),
                "session-ssh"=>self.session_ssh_form(ctx,self.targets_locked()||active(text(&self.current,"status"))),
                _=>html!{<form class="setup" onsubmit={ctx.link().callback(|e:SubmitEvent|{e.prevent_default();Msg::StageSsh})}>
                    {self.field(ctx,"new_ssh_name","SSH executor name","Build machine")}
                    {self.field(ctx,"new_ssh_destination","SSH destination","user@host")}
                    {self.field(ctx,"new_ssh_cwd","Remote working directory","/workspace")}
                    {self.field(ctx,"new_ssh_port","SSH port (optional)","22")}
                    {self.field(ctx,"new_ssh_identity","Identity file on this server (optional)","/home/user/.ssh/id_ed25519")}
                    {self.field(ctx,"new_ssh_known_hosts","Known hosts file on this server (optional)","/home/user/.ssh/known_hosts")}

                    <button class="primary" disabled={self.busy||self.saved.field("new_ssh_name").trim().is_empty()||self.saved.field("new_ssh_destination").trim().is_empty()||!self.saved.field("new_ssh_cwd").starts_with('/')}>{"Add to session"}</button>
                </form>}
            }}
        </crate::modal::Modal>}}else{Html::default()}}
        </>}
    }
    pub(super) fn new_ssh_payload(&self) -> demodex_protocol::SshTarget {
        demodex_protocol::SshTarget {
            name:self.saved.field("new_ssh_name"),destination:self.saved.field("new_ssh_destination"),
            cwd:self.saved.field("new_ssh_cwd"),
            port:nonempty(self.saved.field("new_ssh_port")).map(|p|p.parse::<u16>().unwrap_or(0)),
            identity_file:nonempty(self.saved.field("new_ssh_identity")),
            known_hosts_file:nonempty(self.saved.field("new_ssh_known_hosts")),
        }
    }
    pub(super) fn create_vm_form(&self, ctx: &Context<Self>) -> Html {
        let payload = demodex_protocol::NewEnvironment {
            name: self.saved.field("environment_name"),
            memory_mib: self.saved.field("memory").parse::<u32>().unwrap_or(4096),
            cpus: self.saved.field("cpus").parse::<u16>().unwrap_or(2),
            internet: self.saved.field("internet") == "true",
        };
        html! {<form class="setup" onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Run(Operation::CreateEnvironment{input:payload.clone()})})}>{self.field(ctx,"environment_name","Environment name","Scratch workspace")}<div class="resource-fields"><label>{"Memory (MiB)"}<input type="number" min="512" max="65536" step="1" placeholder="4096" value={self.saved.field("memory")} oninput={ctx.link().callback(|e|Msg::Field("memory".into(),input(e)))}/></label><label>{"CPUs"}<input type="number" min="1" max="32" step="1" placeholder="2" value={self.saved.field("cpus")} oninput={ctx.link().callback(|e|Msg::Field("cpus".into(),input(e)))}/></label></div><label class="checkbox"><input type="checkbox" checked={self.saved.field("internet")=="true"} onchange={ctx.link().callback(|e:Event|Msg::Field("internet".into(),e.target_unchecked_into::<HtmlInputElement>().checked().to_string()))}/>{"Internet and LAN access"}</label><button class="primary" disabled={self.busy||!self.connected}>{"Create and start"}</button></form>}
    }
    pub(super) fn rename_session_view(&self, ctx: &Context<Self>) -> Html {
        let id = self.saved.selected.clone();
        let key = format!("rename:{id}");
        let name = self.saved.fields.get(&key).cloned().unwrap_or_else(|| text(&self.current, "name").into());
        let disabled = self.busy || !self.connected;
        let invalid = name.trim().is_empty() || name.trim().chars().count() > 120;
        let operation = Operation::RenameSession { id, name: name.clone() };
        html! {<section class="control-section" aria-label="Session name">
            <crate::ui::SectionTitle>{"Session name"}</crate::ui::SectionTitle>
            <form onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Run(operation.clone())})}>
                <label>{"Name in Demodex"}<input value={name} disabled={disabled} oninput={ctx.link().callback(move |e|Msg::Field(key.clone(),input(e)))}/></label>
                <button type="submit" disabled={disabled || invalid}>{"Rename session"}</button>
            </form>
        </section>}
    }
    fn resume_settings(&self, ctx: &Context<Self>) -> Html {
        html!{<section class="resume-settings">
            <button type="button" onclick={ctx.link().callback(|_|Msg::SavedSearch(true))}>{"🔍 Search sessions"}</button>
            {self.field(ctx,"session_name","Session name (optional)","Keep saved Codex name")}
            {self.field(ctx,"thread_id","Existing Codex thread ID","Select a saved session")}
            <crate::ui::SectionTitle>{"Working directory"}</crate::ui::SectionTitle><crate::ui::FieldAction>{self.field(ctx,"cwd","Working directory (optional)","Keep saved directory")}
            <crate::directory_picker::DirectoryPicker key={self.generation} client={self.connected.then(||self.client.clone()).flatten()} target="host" path={if self.saved.field("cwd").is_empty(){text(&self.runtime,"workspace").to_owned()}else{self.saved.field("cwd")}} onchoose={ctx.link().callback(|path:String|Msg::Field("cwd".into(),path))}/></crate::ui::FieldAction>
            <crate::ui::SectionTitle>{"Permissions"}</crate::ui::SectionTitle>{self.sandbox(ctx,"sandbox","Resume sandbox")}
            <p class="muted">{"Exit an external CLI session before attaching its saved thread here."}</p>
            {if text(&self.runtime,"mode")!="host"{html!{<p class="error">{"Resuming saved threads requires a configured host runtime."}</p>}}else{Html::default()}}
        </section>}
    }
}
