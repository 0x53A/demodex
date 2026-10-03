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
        let ssh = self.staged_ssh.is_some();
        let chosen = self.new_session_targets();
        let unrestricted = ssh || chosen
            .iter()
            .any(|target| matches!(text(target, "id").split('-').next(), Some("ssh" | "container")));
        let ready = chosen.iter().all(|target| {
                self
                        .targets
                        .iter()
                        .any(|known| known["id"] == target["id"] && known["available"] == true)
            });
        let sandbox = self.saved.field("new_sandbox");
        let models = array(&self.new_models["data"]);
        // Empty means inherit all model settings from the runtime profile.
        let selected_model = self.saved.field("new_model");
        let selected_record = models
            .iter()
            .find(|model| text(model, "model") == selected_model);
        let selected_effort = if self.saved.field("new_effort").is_empty() {
            selected_record
                .map(|model| text(model, "defaultReasoningEffort"))
                .unwrap_or("")
                .to_owned()
        } else {
            self.saved.field("new_effort")
        };
        let selected_tier = self.saved.field("new_tier");
        let defaults = &self.new_models["defaults"];
        let default_record = models.iter().find(|m|m["model"] == defaults["model"]);
        let inherited_label = |value: Option<&str>| match value.filter(|v|!v.is_empty()) {
            Some(value) => format!("Default ({value})"),
            None if self.new_models.is_null() && self.new_model_error.is_empty() => "Default (loading…)".into(),
            None => "Default (unavailable)".into(),
        };
        let default_model_label = inherited_label(default_record.and_then(|m|m["displayName"].as_str()).filter(|name|!name.is_empty()).or_else(||defaults["model"].as_str()));
        let default_effort_label = inherited_label(defaults["effort"].as_str());
        let tier_id = if selected_model.is_empty() { defaults["serviceTier"].as_str() } else {
            self.new_models["configured_service_tier"].as_str().or_else(||selected_record.and_then(|m|m["defaultServiceTier"].as_str()))
        };
        let tier_record = if selected_model.is_empty() { default_record } else { selected_record };
        let tier_name = tier_record.and_then(|m|m["serviceTiers"].as_array()).and_then(|tiers|tiers.iter().find(|t|t["id"].as_str() == tier_id)).and_then(|t|t["name"].as_str());
        let default_tier_label = inherited_label(if !defaults.is_object() { None } else {
            Some(if matches!(tier_id, None | Some("default")) { "Standard" } else { tier_name.or(tier_id).unwrap_or("Standard") })
        });
        let payload = sandbox_choice(&sandbox).map(|sandbox| demodex_protocol::SelectedSession {
            include_project: match self.saved.field("new_project_instructions").as_str() { "true"=>Some(true),"false"=>Some(false),_=>None },
            name: self.saved.field("new_session_name"),
            targets: chosen
                .iter()
                .map(|target| demodex_protocol::Selection {
                    id: text(target, "id").into(),
                    cwd: text(target, "cwd").into(),
                })
                .collect(),
            sandbox,
            model: (!selected_model.is_empty() && !selected_effort.is_empty()).then(|| demodex_protocol::ModelChoice {
                model: selected_model.clone(),
                effort: selected_effort.clone(),
                service_tier: nonempty(selected_tier.clone()),
            }),
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
            <crate::ui::Form class="setup new-session-form" onsubmit={ctx.link().callback(move |event:SubmitEvent|{event.prevent_default();operation.clone().map_or_else(Msg::InvalidForm,Msg::Run)})} actions={html!{<button class="primary" type="submit" disabled={self.busy||!self.connected||self.runtime["running"]!=true||self.runtime["account"].is_null()||if resume{text(&self.runtime,"mode")!="host"}else{(!selected_model.is_empty()&&(selected_record.is_none()||selected_effort.is_empty()))||!ready||(unrestricted&&sandbox!="danger-full-access")}}>{if resume{"Resume session"}else{"Create session"}}</button>}}>
                <fieldset disabled={self.busy||!self.connected}>
                    <crate::ui::Group title="Session">
                    <div class="resume-actions"><label class="checkbox"><input type="checkbox" checked={resume} onchange={ctx.link().callback(|e:Event|Msg::Field("resume_session".into(),e.target_unchecked_into::<HtmlInputElement>().checked().to_string()))}/>{"Resume existing session"}</label>{if resume {html!{<button type="button" onclick={ctx.link().callback(|_|Msg::SavedSearch(true))}>{"🔍 Search sessions"}</button>}}else{Html::default()}}</div>
                    {if resume {html!{<>
                        {self.field(ctx,"session_name","Session name (optional)","Keep saved Codex name")}
                        {self.field(ctx,"thread_id","Existing Codex thread ID","Select a saved session")}
                    </>}}else{self.field(ctx,"new_session_name","Session name (optional)","Untitled session")}}
                    </crate::ui::Group>
                    {if resume {self.resume_settings(ctx)}else{html!{<>
                    <crate::ui::Group title="Executors">
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
                            {if enabled && target["kind"]=="host" {self.new_target_directory(ctx,&chosen,chosen.iter().position(|value|text(value,"id")==id).unwrap())}else{Html::default()}}
                        </div>}
                    })}
                    {if let Some(ssh)=&self.staged_ssh{html!{<div class="staged-ssh"><span>{format!("SSH · {} · {}",ssh.name,ssh.destination)}</span><code>{&ssh.cwd}</code><crate::ui::IconButton label="Remove SSH executor" title="Remove executor" onclick={ctx.link().callback(|_|Msg::RemoveStagedSsh)}>{"×"}</crate::ui::IconButton></div>}}else{Html::default()}}
                    {for chosen.iter().enumerate().filter(|(_,target)|!self.targets.iter().any(|known|known["id"]==target["id"] && known["kind"]=="host")).map(|(index,_)|self.new_target_directory(ctx,&chosen,index))}
                    <div class="target-add-actions">
                        <crate::ui::AddButton onclick={ctx.link().callback(|_|Msg::NewTargetSetup("vm".into()))}>{"+ New VM"}</crate::ui::AddButton>
                        <crate::ui::AddButton onclick={ctx.link().callback(|_|Msg::NewTargetSetup("container".into()))}>{"+ New container"}</crate::ui::AddButton>
                        <crate::ui::AddButton onclick={ctx.link().callback(|_|Msg::NewTargetSetup("ssh".into()))}>{"+ Add SSH"}</crate::ui::AddButton>
                    </div>
                    </crate::ui::Group><crate::ui::Group title="Model settings">
                    <label>{"Model"}<select aria-label="Model" value={selected_model.clone()} onchange={ctx.link().callback(|e:Event|Msg::NewModel(e.target_unchecked_into::<HtmlSelectElement>().value()))}>
                        <option value="" selected={selected_model.is_empty()}>{default_model_label}</option>
                        {for models.iter().map(|model|html!{<option value={text(model,"model").to_owned()} selected={text(model,"model")==selected_model}>{text(model,"displayName")}</option>})}
                    </select></label>
                    <label>{"Reasoning effort"}<select aria-label="Reasoning effort" disabled={selected_model.is_empty()} value={selected_effort.clone()} onchange={ctx.link().callback(|e:Event|Msg::Field("new_effort".into(),e.target_unchecked_into::<HtmlSelectElement>().value()))}>
                        {if selected_model.is_empty(){html!{<option value="">{default_effort_label}</option>}}else{Html::default()}}
                        {for selected_record.into_iter().flat_map(|model|array(&model["supportedReasoningEfforts"])).map(|effort|{let value=text(&effort,"reasoningEffort");html!{<option value={value.to_owned()} selected={value==selected_effort}>{value}</option>}})}
                    </select></label>
                    <label>{"Service tier"}<select aria-label="Service tier" disabled={selected_model.is_empty()} value={selected_tier.clone()} onchange={ctx.link().callback(|e:Event|Msg::Field("new_tier".into(),e.target_unchecked_into::<HtmlSelectElement>().value()))}>
                        <option value="" selected={selected_tier.is_empty()}>{default_tier_label}</option>
                        {for selected_record.into_iter().flat_map(|model|array(&model["serviceTiers"])).filter(|tier|text(tier,"id")!="default").map(|tier|html!{<option value={text(&tier,"id").to_owned()} selected={text(&tier,"id")==selected_tier}>{if text(&tier,"name").is_empty(){text(&tier,"id")}else{text(&tier,"name")}}</option>})}
                    </select></label>
                    {if let Some(error)=self.new_models["defaults_error"].as_str(){html!{<p class="error" role="alert">{error}</p>}}else{Html::default()}}
                    {if !self.new_model_error.is_empty(){html!{<p class="error" role="alert">{&self.new_model_error}<button type="button" onclick={ctx.link().callback(|_|Msg::LoadNewModels)}>{"Retry models"}</button></p>}}else if models.is_empty(){html!{<p role="status">{"Loading models…"}</p>}}else{Html::default()}}
                    </crate::ui::Group><crate::ui::Group title="Permissions">
                    {self.sandbox(ctx,"new_sandbox","Sandbox")}
                    {if unrestricted && sandbox!="danger-full-access"{html!{<p class="muted control-warning" role="status">{"SSH and container executors require danger-full-access."}</p>}}else{Html::default()}}
                    </crate::ui::Group>
                    <crate::ui::Group title="Instructions">
                    <label class="checkbox"><input type="checkbox" checked={self.saved.field("new_project_instructions").is_empty()} onchange={ctx.link().callback({let inherited=self.runtime["prompt_defaults"]["include_project"]!=false;move |e:Event|Msg::Field("new_project_instructions".into(),if e.target_unchecked_into::<HtmlInputElement>().checked(){String::new()}else{inherited.to_string()})})}/>{"Use server instruction default"}</label>
                    <label class="checkbox"><input type="checkbox" disabled={self.saved.field("new_project_instructions").is_empty()} checked={if self.saved.field("new_project_instructions").is_empty(){self.runtime["prompt_defaults"]["include_project"]!=false}else{self.saved.field("new_project_instructions")=="true"}} onchange={ctx.link().callback(|e:Event|Msg::Field("new_project_instructions".into(),e.target_unchecked_into::<HtmlInputElement>().checked().to_string()))}/>{"Include project instruction files"}</label>
                    {if let Some(client)=self.client.clone(){if chosen.is_empty(){html!{<crate::prompt_settings::InstructionPreview client={client} target={None::<demodex_protocol::Selection>}/>}}else{html!{<>{for chosen.iter().map(|t|html!{<div role="group" aria-label={format!("Instructions · {}",text(t,"cwd"))}><p class="muted">{text(t,"cwd")}</p><crate::prompt_settings::InstructionPreview client={client.clone()} target={Some(demodex_protocol::Selection{id:text(t,"id").into(),cwd:text(t,"cwd").into()})}/></div>})}</>}}}else{Html::default()}}
                    </crate::ui::Group></>}}}

                </fieldset>
            </crate::ui::Form>
        </crate::modal::Modal>
        {if self.show_saved_search{html!{<crate::modal::Modal title="Search sessions" stable_size=true onclose={ctx.link().callback(|_|Msg::SavedSearch(false))}>
            {self.modal_error(ctx)}
            <crate::ui::Group label="Search filters"><form class="saved-search" onsubmit={ctx.link().callback(|e:SubmitEvent|{e.prevent_default();Msg::LoadSaved(false)})}>
                <label for="saved-session-search">{"Search saved sessions"}</label><div class="search-input"><input id="saved-session-search" type="search" value={self.saved.field("search")} placeholder="Title or session ID…" oninput={ctx.link().callback(|e|Msg::Field("search".into(),input(e)))}/><button type="submit" disabled={!self.connected||self.busy}>{"🔍 Search"}</button></div>
            </form></crate::ui::Group>
            <div class="picker-results">
            {for self.saved_threads.iter().map(|thread|{let t=thread.clone();let name=thread["name"].as_str().filter(|s|!s.is_empty()).unwrap_or_else(||text(thread,"preview"));html!{
                <button class="session saved-thread" onclick={ctx.link().callback(move |_|Msg::ChooseThread(t.clone()))}><strong>{name}</strong><small>{text(thread,"cwd")}</small><small>{text(thread,"id")}</small></button>
            }})}
            {if self.saved_search_loading{html!{<p role="status">{"Searching saved sessions…"}</p>}}else{Html::default()}}
            {if self.cursor.is_some(){html!{<button disabled={self.saved_search_loading||!self.connected} onclick={ctx.link().callback(|_|Msg::LoadSaved(true))}>{"Load more"}</button>}}else{Html::default()}}
            </div>
        </crate::modal::Modal>}}else{Html::default()}}
        </>}
    }
    fn new_target_directory(&self, ctx: &Context<Self>, chosen: &[Value], index: usize) -> Html {
        let target=&chosen[index];
        let known=self.targets.iter().find(|known|known["id"]==target["id"]);
        let name=known.map(|known|text(known,"name")).unwrap_or("Unavailable target");
        let edited=chosen.to_vec();let mut removed=chosen.to_vec();removed.remove(index);let removed=json!(removed).to_string();
        let mut primary=chosen.to_vec();let target_entry=primary.remove(index);primary.insert(0,target_entry);let primary=json!(primary).to_string();
        html!{<div class="target-directory"><crate::ui::FieldAction><label>{format!("{}{} working directory",name,if index==0{" (primary)"}else{""})}<crate::ui::Input aria_label={format!("{}{} working directory",name,if index==0{" (primary)"}else{""})} value={text(target,"cwd").to_owned()} oninput={ctx.link().callback(move |e:InputEvent|{let mut next=edited.clone();next[index]["cwd"]=json!(input(e));Msg::Field("new_targets".into(),json!(next).to_string())})} rule={crate::ui::Rule::Path}/></label>
        <crate::directory_picker::DirectoryPicker key={format!("{}:{}",self.generation,text(target,"id"))} client={self.connected.then(||self.client.clone()).flatten()} target={text(target,"id").to_owned()} path={text(target,"cwd").to_owned()} onchoose={ctx.link().callback({let targets=chosen.to_vec();let id=text(target,"id").to_owned();move |path:String|{let mut next=targets.clone();if let Some(target)=next.iter_mut().find(|t|t["id"]==id){target["cwd"]=json!(path);}Msg::Field("new_targets".into(),json!(next).to_string())}})}/></crate::ui::FieldAction>
            {if index>0{html!{<button type="button" onclick={ctx.link().callback(move |_|Msg::Field("new_targets".into(),primary.clone()))}>{"Make primary"}</button>}}else{Html::default()}}
            {if known.is_none_or(|known|known["kind"]!="host"){html!{<crate::ui::IconButton label={format!("Remove {name}")} title="Remove executor" onclick={ctx.link().callback(move |_|Msg::Field("new_targets".into(),removed.clone()))}>{"×"}</crate::ui::IconButton>}}else{Html::default()}}
        </div>}
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
                _=>html!{<crate::ui::Form class="setup" onsubmit={ctx.link().callback(|e:SubmitEvent|{e.prevent_default();Msg::StageSsh})} actions={html!{<button class="primary" disabled={self.busy||!self.connected}>{"Add to session"}</button>}}>
                    <crate::ui::Group title="Executor">{self.field(ctx,"new_ssh_name","SSH executor name","Build machine")}
                    {self.field(ctx,"new_ssh_destination","SSH destination","user@host")}
                    {self.field(ctx,"new_ssh_cwd","Remote working directory","/workspace")}
                    </crate::ui::Group><crate::ui::Group title="SSH connection">{self.field(ctx,"new_ssh_port","SSH port (optional)","22")}
                    {self.field(ctx,"new_ssh_identity","Identity file on this server (optional)","/home/user/.ssh/id_ed25519")}
                    {self.field(ctx,"new_ssh_known_hosts","Known hosts file on this server (optional)","/home/user/.ssh/known_hosts")}

                    </crate::ui::Group>
                </crate::ui::Form>}
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
        html! {<crate::ui::Form class="setup" onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Run(Operation::CreateEnvironment{input:payload.clone()})})} actions={html!{<button class="primary" disabled={self.busy||!self.connected}>{"Create and start"}</button>}}><crate::ui::Group title="Environment">{self.field(ctx,"environment_name","Environment name","Scratch workspace")}</crate::ui::Group><crate::ui::Group title="Resources"><div class="resource-fields"><label>{"Memory (MiB)"}<crate::ui::Input kind="number" min="512" max="65536" step="1" placeholder="4096" value={self.saved.field("memory")} oninput={ctx.link().callback(|e|Msg::Field("memory".into(),input(e)))} aria_label="Memory (MiB)" rule={crate::ui::Rule::Text}/></label><label>{"CPUs"}<crate::ui::Input kind="number" min="1" max="32" step="1" placeholder="2" value={self.saved.field("cpus")} oninput={ctx.link().callback(|e|Msg::Field("cpus".into(),input(e)))} aria_label="CPUs" rule={crate::ui::Rule::Text}/></label></div><label class="checkbox"><input type="checkbox" checked={self.saved.field("internet")=="true"} onchange={ctx.link().callback(|e:Event|Msg::Field("internet".into(),e.target_unchecked_into::<HtmlInputElement>().checked().to_string()))}/>{"Internet and LAN access"}</label></crate::ui::Group></crate::ui::Form>}
    }
    pub(super) fn rename_session_view(&self, ctx: &Context<Self>) -> Html {
        let id = self.saved.selected.clone();
        let key = format!("rename:{id}");
        let name = self.saved.fields.get(&key).cloned().unwrap_or_else(|| text(&self.current, "name").into());
        let disabled = self.busy || !self.connected;
        let operation = Operation::RenameSession { id, name: name.clone() };
        html! {<crate::ui::Group class="control-section" label="Session name" title="Session name">

            <crate::ui::Form onsubmit={ctx.link().callback(move |e:SubmitEvent|{e.prevent_default();Msg::Run(operation.clone())})} actions={html!{<button type="submit" disabled={disabled}>{"Rename session"}</button>}}>
                <label>{"Name in Demodex"}<crate::ui::Input value={name} disabled={disabled} oninput={ctx.link().callback(move |e|Msg::Field(key.clone(),input(e)))} aria_label="Name in Demodex" rule={crate::ui::Rule::Name}/></label>

            </crate::ui::Form>
        </crate::ui::Group>}
    }
    fn resume_settings(&self, ctx: &Context<Self>) -> Html {
        html!{<section class="resume-settings">
            <crate::ui::Group title="Working directory"><crate::ui::FieldAction>{self.field(ctx,"cwd","Working directory (optional)","Keep saved directory")}
            <crate::directory_picker::DirectoryPicker key={self.generation} client={self.connected.then(||self.client.clone()).flatten()} target="host" path={if self.saved.field("cwd").is_empty(){text(&self.runtime,"workspace").to_owned()}else{self.saved.field("cwd")}} onchoose={ctx.link().callback(|path:String|Msg::Field("cwd".into(),path))}/></crate::ui::FieldAction>
            </crate::ui::Group><crate::ui::Group title="Permissions">{self.sandbox(ctx,"sandbox","Resume sandbox")}</crate::ui::Group>
            {if text(&self.runtime,"mode")!="host"{html!{<p class="error">{"Resuming saved threads requires a configured host runtime."}</p>}}else{Html::default()}}
        </section>}
    }
}
