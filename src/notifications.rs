//! Durable, explicit agent notifications and bounded, at-most-once Web Push attempts.
use crate::store::{Session, Store};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use demodex_protocol::PushRegistration;
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationInput {
    pub message: String,
    pub title: Option<String>,
}

pub fn initialize(connection: &Connection) -> Result<()> {
    let db = connection.unchecked_transaction()?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS push_identity (id INTEGER PRIMARY KEY CHECK(id=1), server_id TEXT NOT NULL, private_key TEXT NOT NULL, public_key TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS push_devices (id TEXT PRIMARY KEY, registration TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS notification_outbox (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, payload TEXT NOT NULL, recipients TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending');")?;
    if db.query_row("SELECT COUNT(*) FROM push_identity", [], |r| {
        r.get::<_, i64>(0)
    })? == 0
    {
        let key = SigningKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
        db.execute(
            "INSERT INTO push_identity VALUES(1,?1,?2,?3)",
            params![
                uuid::Uuid::new_v4().to_string(),
                B64.encode(key.to_bytes()),
                B64.encode(key.verifying_key().to_encoded_point(false).as_bytes())
            ],
        )?;
    }
    // A crashed attempt is uncertain. Never send it again on restart.
    let interrupted = db
        .prepare("SELECT id,session_id FROM notification_outbox WHERE state='sending'")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, session) in interrupted {
        let event = json!({"method":"demodex/notificationDelivery","params":{"id":id,"state":"unavailable","accepted":0,"failed":0,"expired":0,"unavailable":1}});
        db.execute(
            "INSERT INTO events(session_id,message) VALUES(?1,?2)",
            params![session, event.to_string()],
        )?;
        db.execute("DELETE FROM notification_outbox WHERE id=?1", [id])?;
    }
    db.commit()?;
    Ok(())
}

fn identity(store: &Store) -> Result<(String, String, String)> {
    Ok(store.lock()?.query_row(
        "SELECT server_id,private_key,public_key FROM push_identity WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}
fn device(store: &Store, id: &str) -> Result<Option<PushRegistration>> {
    let value: Option<String> = store
        .lock()?
        .query_row(
            "SELECT registration FROM push_devices WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    value
        .map(|v| serde_json::from_str(&v).map_err(Into::into))
        .transpose()
}
pub fn settings(store: &Store, device_id: &str) -> Result<Value> {
    let (server_id, _, public_key) = identity(store)?;
    let registration = device(store, device_id)?;
    Ok(
        json!({"server_id":server_id,"public_key":public_key,"enabled":registration.is_some(),"hide_preview":registration.as_ref().is_some_and(|d|d.hide_preview)}),
    )
}

fn validate_endpoint(endpoint: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(endpoint)?;
    // Do not allow subscription registration to become an arbitrary HTTP client.
    let host = url.host_str().unwrap_or("");
    ensure!(
        endpoint.len() <= 4096
            && url.scheme() == "https"
            && url.port().is_none_or(|p| p == 443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "Invalid HTTPS push endpoint"
    );
    ensure!(
        host == "fcm.googleapis.com"
            || host == "updates.push.services.mozilla.com"
            || host.ends_with(".push.services.mozilla.com")
            || host == "web.push.apple.com"
            || host.ends_with(".notify.windows.com"),
        "Unsupported browser push service"
    );
    Ok(url)
}
fn validate_origin(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)?;
    ensure!(
        (url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))))
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Use an HTTPS URL without credentials, query or fragment"
    );
    Ok(url)
}
pub fn register(store: &Store, input: PushRegistration) -> Result<Value> {
    ensure!(
        uuid::Uuid::parse_str(&input.device_id).is_ok(),
        "Invalid device ID"
    );
    validate_endpoint(&input.endpoint)?;
    let public = B64.decode(&input.p256dh)?;
    ensure!(
        public.len() == 65 && p256::PublicKey::from_sec1_bytes(&public).is_ok(),
        "Invalid push encryption key"
    );
    ensure!(
        B64.decode(&input.auth)?.len() == 16,
        "Invalid push authentication secret"
    );
    let frontend = validate_origin(&input.frontend_url)?;
    ensure!(
        input.frontend_url.len() <= 2048 && frontend.path().ends_with('/'),
        "Frontend URL must be the PWA directory"
    );
    let server = validate_origin(&input.server_url)?;
    ensure!(
        server.path() == "/" && input.server_url.len() <= 2048,
        "Server URL must be an origin"
    );
    let db = store.lock()?;
    let count: i64 = db.query_row("SELECT COUNT(*) FROM push_devices", [], |r| r.get(0))?;
    ensure!(
        count < 64
            || db.query_row(
                "SELECT COUNT(*) FROM push_devices WHERE id=?1",
                [&input.device_id],
                |r| r.get::<_, i64>(0)
            )? > 0,
        "Maximum of 64 push devices reached; disable unused devices first"
    );
    db.execute("INSERT INTO push_devices VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET registration=excluded.registration",params![input.device_id,serde_json::to_string(&input)?])?;
    Ok(json!({"enabled":true}))
}
pub fn remove(store: &Store, id: &str) -> Result<Value> {
    store
        .lock()?
        .execute("DELETE FROM push_devices WHERE id=?1", [id])?;
    Ok(json!({"enabled":false}))
}

pub fn record(tx: &Connection, session: &Session, input: NotificationInput) -> Result<Value> {
    ensure!(
        !input.message.trim().is_empty()
            && input.message.chars().count() <= 1000
            && !input
                .message
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t'),
        "message must be 1–1000 characters without control characters"
    );
    if let Some(title) = &input.title {
        ensure!(
            !title.trim().is_empty()
                && title.chars().count() <= 120
                && !title.chars().any(char::is_control),
            "title must be 1–120 characters on one line"
        );
    }
    let id = uuid::Uuid::new_v4().to_string();
    let payload = json!({"id":id,"session_id":session.id,"title":input.title.unwrap_or_else(||session.name.chars().take(120).collect()),"message":input.message,"session_name":session.name});
    let recipients = tx
        .prepare("SELECT id FROM push_devices ORDER BY id")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    tx.execute_batch("SAVEPOINT notification_record")?;
    let written = (|| -> Result<()> {
        tx.execute(
            "INSERT INTO events(session_id,message) VALUES(?1,?2)",
            params![
                session.id,
                json!({"method":"demodex/notification","params":payload}).to_string()
            ],
        )?;
        tx.execute(
            "INSERT INTO notification_outbox(id,session_id,payload,recipients) VALUES(?1,?2,?3,?4)",
            params![
                id,
                session.id,
                payload.to_string(),
                serde_json::to_string(&recipients)?
            ],
        )?;
        Ok(())
    })();
    if let Err(error) = written {
        tx.execute_batch("ROLLBACK TO notification_record; RELEASE notification_record")?;
        return Err(error);
    }
    tx.execute_batch("RELEASE notification_record")?;
    Ok(
        json!({"notification_id":id,"recorded_in_chat":true,"push_devices":recipients.len(),"push_state":if recipients.is_empty(){"disabled"}else{"queued"},"message":"Recorded in chat. Push is best effort; this does not confirm receipt or reading."}),
    )
}

#[derive(Default, Serialize)]
struct Outcome {
    accepted: u64,
    failed: u64,
    expired: u64,
    unavailable: u64,
}
fn encrypted_request(
    reg: &PushRegistration,
    payload: &Value,
    private: &str,
    public: &str,
) -> Result<(String, Vec<u8>)> {
    let endpoint = validate_endpoint(&reg.endpoint)?;
    let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims=B64.encode(serde_json::to_vec(&json!({"aud":endpoint.origin().ascii_serialization(),"exp":SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()+3600,"sub":reg.frontend_url}))?);
    let signing_input = format!("{header}.{claims}");
    let key = SigningKey::from_slice(&B64.decode(private)?)?;
    let signature: Signature = key.sign(signing_input.as_bytes());
    let authorization = format!(
        "vapid t={signing_input}.{}, k={public}",
        B64.encode(signature.to_bytes())
    );
    let bytes = serde_json::to_vec(payload)?;
    ensure!(bytes.len() <= 3900, "Notification payload is too large");
    let body = ece::encrypt(&B64.decode(&reg.p256dh)?, &B64.decode(&reg.auth)?, &bytes)
        .context("Push encryption failed")?;
    Ok((authorization, body))
}
fn payload(reg: &PushRegistration, notification: &Value, server_id: &str) -> Value {
    let mut result = notification.clone();
    result["server_id"] = json!(server_id);
    result["server_url"] = json!(reg.server_url);
    // Scope is local to the installation; no authentication data is ever included.
    if let (Some(name), Some(title)) = (
        notification["session_name"].as_str(),
        notification["title"].as_str(),
    ) && name != title
    {
        result["title"] = json!(format!(
            "{} · {title}",
            name.chars().take(120).collect::<String>()
        ));
    }
    result.as_object_mut().unwrap().remove("session_name");
    if reg.hide_preview {
        result["title"] = json!("Demodex");
        result["message"] = json!("A session sent a notification.");
    }
    // UTF-8 worst case + routing fields can exceed one Web Push record. Truncate
    // previews only; the full original always remains in the chat event.
    while serde_json::to_vec(&result).map_or(usize::MAX, |v| v.len()) > 3800 {
        let mut message = result["message"].as_str().unwrap_or("").to_owned();
        if message.is_empty() {
            break;
        }
        message.pop();
        result["message"] = json!(message);
    }
    result
}
async fn deliver(store: &Store, ids: Vec<String>, notification: &Value) -> Result<Outcome> {
    let (server_id, private, public) = identity(store)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .build()?;
    let mut outcome = Outcome::default();
    use futures_util::{StreamExt, stream};
    let attempts = stream::iter(ids)
        .map(|id| {
            let client = &client;
            let private = &private;
            let public = &public;
            let server_id = &server_id;
            async move {
                let Some(reg) = device(store, &id)? else {
                    return Ok::<_, anyhow::Error>((id, None, None));
                };
                let data = payload(&reg, notification, server_id);
                let (authorization, body) = encrypted_request(&reg, &data, private, public)?;
                let status = client
                    .post(&reg.endpoint)
                    .header("Authorization", authorization)
                    .header("Content-Encoding", "aes128gcm")
                    .header("Content-Type", "application/octet-stream")
                    .header("TTL", "86400")
                    .header("Urgency", "normal")
                    .body(body)
                    .send()
                    .await
                    .map(|r| r.status().as_u16());
                Ok((id, Some(serde_json::to_string(&reg)?), Some(status)))
            }
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    for attempt in attempts {
        match attempt {
            Ok((_, _, Some(Ok(200..=299)))) => outcome.accepted += 1,
            Ok((id, Some(registration), Some(Ok(404 | 410)))) => {
                // Compare and delete under one database lock: a replacement may
                // have registered while this request was in flight.
                remove_expired(store, &id, &registration)?;
                outcome.expired += 1;
            }
            Ok((_, _, None)) => outcome.unavailable += 1,
            Ok((_, _, Some(Err(_)))) => outcome.unavailable += 1,
            _ => outcome.failed += 1,
        }
    }
    Ok(outcome)
}
fn remove_expired(store: &Store, id: &str, expected: &str) -> Result<()> {
    store.lock()?.execute(
        "DELETE FROM push_devices WHERE id=?1 AND registration=?2",
        params![id, expected],
    )?;
    Ok(())
}
pub async fn test(store: &Store, id: &str) -> Result<Value> {
    ensure!(
        device(store, id)?.is_some(),
        "Enable push on this device first"
    );
    let data = json!({"id":uuid::Uuid::new_v4().to_string(),"session_id":"","title":"Demodex test","message":"Push notifications are enabled on this device."});
    let outcome = deliver(store, vec![id.into()], &data).await?;
    Ok(
        json!({"delivery":outcome,"message":"Accepted means the push service accepted the request, not that the device displayed it."}),
    )
}
pub async fn drain(manager: &crate::manager::Manager) -> Result<()> {
    // Claim before any I/O. An interrupted claim is never replayed.
    let job = {
        let mut db = manager.store.lock()?;
        let tx = db.transaction()?;
        let job:Option<(String,String,String,String)>=tx.query_row("SELECT id,session_id,payload,recipients FROM notification_outbox WHERE state='pending' ORDER BY rowid LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if let Some((id, _, _, _)) = &job {
            tx.execute(
                "UPDATE notification_outbox SET state='sending' WHERE id=?1",
                [id],
            )?;
        }
        tx.commit()?;
        job
    };
    let Some((id, session, data, ids)) = job else {
        return Ok(());
    };
    let recipients: Vec<String> = serde_json::from_str(&ids)?;
    let count = recipients.len();
    // Bound the entire batch as well as each HTTP request.
    let outcome = match tokio::time::timeout(
        Duration::from_secs(20),
        deliver(&manager.store, recipients, &serde_json::from_str(&data)?),
    )
    .await
    {
        Ok(Ok(outcome)) => outcome,
        _ => Outcome {
            unavailable: count as u64,
            ..Default::default()
        },
    };
    let state = if count == 0 {
        "disabled"
    } else if outcome.unavailable > 0 {
        "unavailable"
    } else if outcome.failed > 0 || outcome.expired > 0 {
        "partial-or-failed"
    } else {
        "accepted"
    };
    let mut params = serde_json::to_value(outcome)?;
    params["id"] = json!(id);
    params["state"] = json!(state);
    let mut db = manager.store.lock()?;
    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO events(session_id,message) VALUES(?1,?2)",
        params![
            session,
            json!({"method":"demodex/notificationDelivery","params":params}).to_string()
        ],
    )?;
    tx.execute("DELETE FROM notification_outbox WHERE id=?1", [id])?;
    tx.commit()?;
    manager.session_changed(&session, false);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{VerifyingKey, signature::Verifier};
    fn registration() -> Result<(PushRegistration, Box<dyn ece::LocalKeyPair>, [u8; 16])> {
        let (key, auth) = ece::generate_keypair_and_auth_secret()?;
        Ok((
            PushRegistration {
                device_id: uuid::Uuid::new_v4().to_string(),
                endpoint: "https://fcm.googleapis.com/fcm/send/test".into(),
                p256dh: B64.encode(key.pub_as_raw()?),
                auth: B64.encode(auth),
                frontend_url: "https://app.example/demodex/".into(),
                server_url: "https://daemon.example".into(),
                hide_preview: false,
            },
            key,
            auth,
        ))
    }
    #[test]
    fn push_encrypts_and_signs_for_the_browser_subscription() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(&dir.path().join("state.sqlite"))?;
        let (server_id, private, public) = identity(&store)?;
        let (reg, key, auth) = registration()?;
        let notification = json!({"id":"notification","session_id":"session","title":"Done","session_name":"Raven","message":"Private result"});
        let data = payload(&reg, &notification, &server_id);
        assert_eq!(data["title"], "Raven · Done");
        let (authorization, body) = encrypted_request(&reg, &data, &private, &public)?;
        assert!(!body.windows(14).any(|s| s == b"Private result"));
        let decrypted = ece::decrypt(&key.raw_components()?, &auth, &body)?;
        assert_eq!(serde_json::from_slice::<Value>(&decrypted)?, data);
        let jwt = authorization
            .strip_prefix("vapid t=")
            .unwrap()
            .split(", k=")
            .next()
            .unwrap();
        let parts = jwt.split('.').collect::<Vec<_>>();
        assert_eq!(parts.len(), 3);
        let claims: Value = serde_json::from_slice(&B64.decode(parts[1])?)?;
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["sub"], reg.frontend_url);
        VerifyingKey::from_sec1_bytes(&B64.decode(&public)?)?.verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &Signature::from_slice(&B64.decode(parts[2])?)?,
        )?;
        let mut hidden = reg.clone();
        hidden.hide_preview = true;
        let hidden = payload(&hidden, &notification, &server_id);
        assert_eq!(hidden["title"], "Demodex");
        assert!(!hidden.to_string().contains("Private result"));
        assert!(!hidden.to_string().contains("Raven"));
        Ok(())
    }
    #[tokio::test]
    async fn notification_receipt_is_atomic_durable_and_never_replays_interrupted_push()
    -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        let store = Store::open(&path)?;
        let session = store.create("Raven", "ws://localhost:1", &[], Some("thread"))?;
        store.enable_context_reporting(&session.id)?;
        let call = json!({"namespace":"demodex","tool":"notify","threadId":"thread","callId":"notification-call","arguments":{"message":"Done"}});
        let first = store.context_tool(&session.id, "thread", &call)?;
        assert_eq!(first["success"], true);
        assert_eq!(store.context_tool(&session.id, "thread", &call)?, first);
        assert_eq!(store.events(&session.id, 0)?.len(), 1);
        let mut different = call.clone();
        different["arguments"]["message"] = json!("Changed");
        assert!(
            store
                .context_tool(&session.id, "thread", &different)
                .is_err()
        );
        let identity_before = identity(&store)?;
        store
            .lock()?
            .execute("UPDATE notification_outbox SET state='sending'", [])?;
        drop(store);
        let store = Store::open(&path)?;
        assert_eq!(identity(&store)?, identity_before);
        assert_eq!(store.context_tool(&session.id, "thread", &call)?, first);
        assert_eq!(store.events(&session.id, 0)?.len(), 2);
        assert_eq!(
            store.events(&session.id, 0)?[1].message["params"]["state"],
            "unavailable"
        );
        assert_eq!(
            store
                .lock()?
                .query_row("SELECT COUNT(*) FROM notification_outbox", [], |r| r
                    .get::<_, i64>(0))?,
            0
        );
        store.lock()?.execute_batch("CREATE TRIGGER fail_outbox BEFORE INSERT ON notification_outbox BEGIN SELECT RAISE(FAIL,'fixture failure'); END;")?;
        let mut failed = call.clone();
        failed["callId"] = json!("failed-insert");
        assert_eq!(
            store.context_tool(&session.id, "thread", &failed)?["success"],
            false
        );
        assert_eq!(store.events(&session.id, 0)?.len(), 2);
        store.lock()?.execute_batch("DROP TRIGGER fail_outbox")?;
        let mut next = call.clone();
        next["callId"] = json!("next");
        store.context_tool(&session.id, "thread", &next)?;
        let manager = crate::manager::Manager::new(store);
        drain(&manager).await?;
        assert_eq!(
            manager.store.events(&session.id, 0)?[3].message["params"]["state"],
            "disabled"
        );
        Ok(())
    }
    #[test]
    fn subscriptions_are_validated_private_and_removable() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(&dir.path().join("state.sqlite"))?;
        let (reg, _, _) = registration()?;
        register(&store, reg.clone())?;
        let public = settings(&store, &reg.device_id)?;
        assert_eq!(public["enabled"], true);
        assert!(!public.to_string().contains(&reg.auth));
        assert!(!public.to_string().contains("fcm/send"));
        for endpoint in [
            "http://fcm.googleapis.com/x",
            "https://localhost/push",
            "https://fcm.googleapis.com.evil.test/x",
            "https://fcm.googleapis.com:444/x",
            "https://user:secret@fcm.googleapis.com/x",
        ] {
            let mut bad = reg.clone();
            bad.endpoint = endpoint.into();
            assert!(register(&store, bad).is_err());
        }
        let original = serde_json::to_string(&reg)?;
        let mut replacement = reg.clone();
        replacement.hide_preview = true;
        register(&store, replacement.clone())?;
        remove_expired(&store, &reg.device_id, &original)?;
        assert_eq!(settings(&store, &reg.device_id)?["enabled"], true);
        remove_expired(
            &store,
            &reg.device_id,
            &serde_json::to_string(&replacement)?,
        )?;
        assert_eq!(settings(&store, &reg.device_id)?["enabled"], false);
        register(&store, reg.clone())?;
        remove(&store, &reg.device_id)?;
        assert_eq!(settings(&store, &reg.device_id)?["enabled"], false);
        Ok(())
    }
}
