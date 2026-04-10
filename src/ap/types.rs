//! Minimal ActivityPub / ActivityStreams JSON-LD types.
//!
//! The fediverse uses JSON-LD as "JSON with a @context declaration" — we do not
//! perform full JSON-LD expansion. We deserialize using compacted key names
//! directly. Unknown fields are captured in `extra` where needed.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Deserialize a JSON value that can be either a single item or an array.
/// Some AP implementations (e.g. GTS 0.20) send a single object instead of
/// a one-element array for fields like `tag` and `attachment`.
fn deserialize_one_or_many<'de, D>(d: D) -> Result<Option<Vec<Value>>, D::Error>
where
    D: Deserializer<'de>,
{
    let v: Option<Value> = Option::deserialize(d)?;
    match v {
        None => Ok(None),
        Some(Value::Array(a)) => Ok(Some(a)),
        Some(single) => Ok(Some(vec![single])),
    }
}

// ── @context helpers ─────────────────────────────────────────────────────────

/// The standard @context array used on our actor document.
pub fn actor_context() -> Value {
    serde_json::json!([
        "https://www.w3.org/ns/activitystreams",
        "https://w3id.org/security/v1",
        {
            "manuallyApprovesFollowers": "as:manuallyApprovesFollowers",
            "toot": "http://joinmastodon.org/ns#",
            "discoverable": "toot:discoverable",
            "schema": "http://schema.org/",
            "PropertyValue": "schema:PropertyValue",
            "value": "schema:value"
        }
    ])
}

// ── Incoming activity envelope ───────────────────────────────────────────────

/// A raw incoming Activity from the inbox. We keep `object` as raw JSON
/// because it can be a URL string, an embedded object, or an array.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct IncomingActivity {
    #[serde(rename = "@context")]
    pub context: Option<Value>,

    pub id: Option<String>,

    #[serde(rename = "type")]
    pub activity_type: String,

    pub actor: StringOrObject,

    pub object: Option<Value>,

    pub to: Option<StringOrArray>,
    pub cc: Option<StringOrArray>,

    // Catch remaining fields for future-proofing
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// A value that is either a plain string (IRI) or an embedded object.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum StringOrObject {
    String(String),
    Object(Value),
}

impl StringOrObject {
    /// Extract the string ID regardless of whether this is a bare IRI or object.
    pub fn id(&self) -> Option<&str> {
        match self {
            StringOrObject::String(s) => Some(s.as_str()),
            StringOrObject::Object(v) => v.get("id")?.as_str(),
        }
    }
}

/// A value that is either a single string or an array of strings.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum StringOrArray {
    Single(String),
    Array(Vec<String>),
}

#[allow(dead_code)]
impl StringOrArray {
    pub fn to_vec(&self) -> Vec<&str> {
        match self {
            StringOrArray::Single(s) => vec![s.as_str()],
            StringOrArray::Array(v) => v.iter().map(String::as_str).collect(),
        }
    }

    pub fn contains(&self, needle: &str) -> bool {
        self.to_vec().contains(&needle)
    }
}

/// The well-known public URI for "public" (addressed to the world).
pub const PUBLIC_URI: &str = "https://www.w3.org/ns/activitystreams#Public";

pub fn is_public(to: &Option<StringOrArray>, cc: &Option<StringOrArray>) -> bool {
    let check = |soa: &StringOrArray| {
        soa.to_vec()
            .iter()
            .any(|u| *u == PUBLIC_URI || *u == "Public" || *u == "as:Public")
    };
    to.as_ref().map(check).unwrap_or(false) || cc.as_ref().map(check).unwrap_or(false)
}

// ── AP Note (the comment payload) ────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct Note {
    pub id: String,

    #[serde(rename = "type")]
    pub note_type: String, // "Note", "Article", etc.

    #[serde(rename = "attributedTo")]
    pub attributed_to: Option<StringOrObject>,

    /// HTML content (the primary representation in AP)
    pub content: Option<String>,

    /// Localised content map; we use the first value if `content` is absent
    #[serde(rename = "contentMap")]
    pub content_map: Option<std::collections::HashMap<String, String>>,

    /// Original source (e.g. Markdown before rendering)
    pub source: Option<NoteSource>,

    #[serde(rename = "inReplyTo")]
    pub in_reply_to: Option<StringOrObject>,

    pub url: Option<StringOrObject>,

    pub to: Option<StringOrArray>,
    pub cc: Option<StringOrArray>,

    pub published: Option<String>,

    pub sensitive: Option<bool>,
    pub summary: Option<String>, // content warning / subject line

    /// Hashtag / Mention tags
    #[serde(default, deserialize_with = "deserialize_one_or_many")]
    pub tag: Option<Vec<Value>>,

    /// Attached media
    #[serde(default, deserialize_with = "deserialize_one_or_many")]
    pub attachment: Option<Vec<Value>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NoteSource {
    pub content: String,
    #[serde(rename = "mediaType")]
    pub media_type: String,
}

#[allow(dead_code)]
impl Note {
    /// Returns the best available HTML content.
    pub fn best_content(&self) -> Option<&str> {
        if let Some(c) = &self.content {
            return Some(c.as_str());
        }
        self.content_map
            .as_ref()
            .and_then(|m| m.values().next())
            .map(String::as_str)
    }

    /// Returns Markdown source if provided and media type is text/markdown.
    pub fn markdown_source(&self) -> Option<&str> {
        self.source.as_ref().and_then(|s| {
            if s.media_type == "text/markdown" || s.media_type == "text/x.misskeymarkdown" {
                Some(s.content.as_str())
            } else {
                None
            }
        })
    }

    /// True if addressed to the public audience.
    pub fn is_public(&self) -> bool {
        is_public(&self.to, &self.cc)
    }

    /// Best URL for displaying this note (prefers `url` over `id`).
    pub fn display_url(&self) -> &str {
        self.url
            .as_ref()
            .and_then(|u| u.id())
            .unwrap_or(self.id.as_str())
    }
}

// ── AP Actor ─────────────────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct RemoteActor {
    pub id: String,

    #[serde(rename = "type")]
    pub actor_type: String, // Person, Service, Application, Group, Organization

    #[serde(rename = "preferredUsername")]
    pub preferred_username: Option<String>,

    pub name: Option<String>,
    pub summary: Option<String>,
    pub url: Option<StringOrObject>,
    pub inbox: Option<String>,

    pub endpoints: Option<ActorEndpoints>,

    #[serde(rename = "publicKey")]
    pub public_key: Option<PublicKeyObject>,

    pub icon: Option<Value>,  // avatar
    pub image: Option<Value>, // header/banner image
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ActorEndpoints {
    #[serde(rename = "sharedInbox")]
    pub shared_inbox: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PublicKeyObject {
    pub id: String,
    pub owner: Option<String>,
    #[serde(rename = "publicKeyPem")]
    pub public_key_pem: String,
}

#[allow(dead_code)]
impl RemoteActor {
    pub fn preferred_inbox(&self) -> Option<&str> {
        self.endpoints
            .as_ref()
            .and_then(|e| e.shared_inbox.as_deref())
            .or(self.inbox.as_deref())
    }

    pub fn avatar_url(&self) -> Option<&str> {
        self.icon.as_ref()?.get("url")?.as_str()
    }

    pub fn profile_url(&self) -> Option<&str> {
        self.url.as_ref().and_then(|u| u.id())
    }
}

// ── Our outbound actor document ───────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ActorDocument {
    #[serde(rename = "@context")]
    pub context: Value,

    pub id: String,

    #[serde(rename = "type")]
    pub actor_type: &'static str,

    #[serde(rename = "preferredUsername")]
    pub preferred_username: String,

    pub name: String,
    pub summary: String,
    pub url: String,

    pub inbox: String,
    pub outbox: String,
    pub followers: String,
    pub following: String,

    pub endpoints: ActorEndpointsOut,

    #[serde(rename = "publicKey")]
    pub public_key: PublicKeyObject,

    #[serde(rename = "manuallyApprovesFollowers")]
    pub manually_approves_followers: bool,

    pub discoverable: bool,

    pub published: String,
}

#[derive(Debug, Serialize)]
pub struct ActorEndpointsOut {
    #[serde(rename = "sharedInbox")]
    pub shared_inbox: String,
}

// ── WebFinger JRD ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct WebFingerResponse {
    pub subject: String,
    pub aliases: Vec<String>,
    pub links: Vec<WebFingerLink>,
}

#[derive(Debug, Serialize)]
pub struct WebFingerLink {
    pub rel: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub link_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── StringOrArray ─────────────────────────────────────────────────────────

    #[test]
    fn string_or_array_single_to_vec() {
        let s = StringOrArray::Single("https://example.com".into());
        assert_eq!(s.to_vec(), vec!["https://example.com"]);
        assert!(s.contains("https://example.com"));
        assert!(!s.contains("https://other.com"));
    }

    #[test]
    fn string_or_array_array_to_vec() {
        let s: StringOrArray = serde_json::from_str(
            r#"["https://www.w3.org/ns/activitystreams#Public","https://example.com/followers"]"#,
        )
        .unwrap();
        assert!(s.contains("https://www.w3.org/ns/activitystreams#Public"));
        assert!(!s.contains("https://other.com"));
    }

    // ── is_public ─────────────────────────────────────────────────────────────

    #[test]
    fn is_public_when_to_contains_public_uri() {
        let to = Some(StringOrArray::Single(
            "https://www.w3.org/ns/activitystreams#Public".into(),
        ));
        assert!(is_public(&to, &None));
    }

    #[test]
    fn is_public_when_cc_contains_public_uri() {
        let cc = Some(StringOrArray::Array(vec![
            "https://example.com/followers".into(),
            "https://www.w3.org/ns/activitystreams#Public".into(),
        ]));
        assert!(is_public(&None, &cc));
    }

    #[test]
    fn not_public_when_missing() {
        assert!(!is_public(&None, &None));
        let to = Some(StringOrArray::Single(
            "https://example.com/users/bob".into(),
        ));
        assert!(!is_public(&to, &None));
    }

    // ── Note deserialization ──────────────────────────────────────────────────

    const MASTODON_NOTE: &str = r#"{
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": "https://mastodon.social/users/alice/statuses/1234",
        "type": "Note",
        "attributedTo": "https://mastodon.social/users/alice",
        "content": "<p>Hello, world!</p>",
        "inReplyTo": null,
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
        "cc": ["https://mastodon.social/users/alice/followers"],
        "published": "2024-01-15T12:00:00Z",
        "sensitive": false,
        "url": "https://mastodon.social/@alice/1234"
    }"#;

    #[test]
    fn deserializes_mastodon_note() {
        let note: Note = serde_json::from_str(MASTODON_NOTE).unwrap();
        assert_eq!(note.id, "https://mastodon.social/users/alice/statuses/1234");
        assert_eq!(note.note_type, "Note");
        assert_eq!(note.best_content(), Some("<p>Hello, world!</p>"));
        assert!(note.is_public());
        assert!(!note.sensitive.unwrap_or(false));
        assert_eq!(note.display_url(), "https://mastodon.social/@alice/1234");
    }

    const GOTOSOCIAL_NOTE: &str = r#"{
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": "https://gts.example/users/bob/statuses/abcdef",
        "type": "Note",
        "attributedTo": "https://gts.example/users/bob",
        "contentMap": {
            "en": "<p>GoToSocial post</p>"
        },
        "source": {
            "content": "GoToSocial post",
            "mediaType": "text/markdown"
        },
        "inReplyTo": "https://mastodon.social/users/alice/statuses/1234",
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
        "cc": [],
        "published": "2024-01-15T13:00:00Z"
    }"#;

    #[test]
    fn deserializes_gotosocial_note_with_content_map() {
        let note: Note = serde_json::from_str(GOTOSOCIAL_NOTE).unwrap();
        // content is absent; best_content() should fall through to contentMap
        assert_eq!(note.best_content(), Some("<p>GoToSocial post</p>"));
        assert_eq!(note.markdown_source(), Some("GoToSocial post"));
        assert!(note.is_public());
        let reply_id = note.in_reply_to.as_ref().and_then(|r| r.id());
        assert_eq!(
            reply_id,
            Some("https://mastodon.social/users/alice/statuses/1234")
        );
    }

    // ── RemoteActor deserialization ───────────────────────────────────────────

    const MASTODON_ACTOR: &str = r#"{
        "@context": [
            "https://www.w3.org/ns/activitystreams",
            "https://w3id.org/security/v1"
        ],
        "id": "https://mastodon.social/users/alice",
        "type": "Person",
        "preferredUsername": "alice",
        "name": "Alice",
        "inbox": "https://mastodon.social/users/alice/inbox",
        "endpoints": {
            "sharedInbox": "https://mastodon.social/inbox"
        },
        "publicKey": {
            "id": "https://mastodon.social/users/alice#main-key",
            "owner": "https://mastodon.social/users/alice",
            "publicKeyPem": "-----BEGIN PUBLIC KEY-----\nMIIB...\n-----END PUBLIC KEY-----\n"
        },
        "icon": {
            "type": "Image",
            "url": "https://mastodon.social/avatars/alice.jpg"
        },
        "url": "https://mastodon.social/@alice"
    }"#;

    #[test]
    fn deserializes_mastodon_actor() {
        let actor: RemoteActor = serde_json::from_str(MASTODON_ACTOR).unwrap();
        assert_eq!(actor.id, "https://mastodon.social/users/alice");
        assert_eq!(actor.actor_type, "Person");
        assert_eq!(actor.preferred_username.as_deref(), Some("alice"));
        assert_eq!(
            actor.inbox.as_deref(),
            Some("https://mastodon.social/users/alice/inbox")
        );
        assert_eq!(
            actor.preferred_inbox(),
            Some("https://mastodon.social/inbox")
        );
        assert_eq!(
            actor.avatar_url(),
            Some("https://mastodon.social/avatars/alice.jpg")
        );
        let pk = actor.public_key.unwrap();
        assert_eq!(pk.id, "https://mastodon.social/users/alice#main-key");
    }

    // ── IncomingActivity deserialization ──────────────────────────────────────

    #[test]
    fn deserializes_follow_activity() {
        let json = r#"{
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": "https://remote.example/follows/1",
            "type": "Follow",
            "actor": "https://remote.example/users/alice",
            "object": "https://test.example/actor"
        }"#;

        let activity: IncomingActivity = serde_json::from_str(json).unwrap();
        assert_eq!(activity.activity_type, "Follow");
        assert_eq!(
            activity.actor.id(),
            Some("https://remote.example/users/alice")
        );
        assert_eq!(
            activity.id.as_deref(),
            Some("https://remote.example/follows/1")
        );
    }

    #[test]
    fn deserializes_create_note_activity() {
        let json = r#"{
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": "https://remote.example/activities/1",
            "type": "Create",
            "actor": "https://remote.example/users/alice",
            "object": {
                "id": "https://remote.example/notes/1",
                "type": "Note",
                "attributedTo": "https://remote.example/users/alice",
                "content": "<p>test</p>",
                "to": ["https://www.w3.org/ns/activitystreams#Public"]
            }
        }"#;

        let activity: IncomingActivity = serde_json::from_str(json).unwrap();
        assert_eq!(activity.activity_type, "Create");
        let obj = activity.object.as_ref().unwrap();
        let note: Note = serde_json::from_value(obj.clone()).unwrap();
        assert_eq!(note.id, "https://remote.example/notes/1");
        assert_eq!(note.best_content(), Some("<p>test</p>"));
    }

    #[test]
    fn deserializes_delete_activity_with_bare_object_url() {
        let json = r#"{
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": "https://remote.example/activities/delete/1",
            "type": "Delete",
            "actor": "https://remote.example/users/alice",
            "object": "https://remote.example/notes/1"
        }"#;

        let activity: IncomingActivity = serde_json::from_str(json).unwrap();
        assert_eq!(activity.activity_type, "Delete");
        let obj = activity.object.as_ref().unwrap();
        assert_eq!(obj.as_str(), Some("https://remote.example/notes/1"));
    }

    #[test]
    fn actor_field_accepts_string_or_object() {
        // String form (most common)
        let json = r#"{"type":"Follow","actor":"https://example.com/users/alice","object":""}"#;
        let a: IncomingActivity = serde_json::from_str(json).unwrap();
        assert_eq!(a.actor.id(), Some("https://example.com/users/alice"));

        // Object form (less common)
        let json = r#"{"type":"Follow","actor":{"id":"https://example.com/users/bob","type":"Person"},"object":""}"#;
        let a: IncomingActivity = serde_json::from_str(json).unwrap();
        assert_eq!(a.actor.id(), Some("https://example.com/users/bob"));
    }
}
