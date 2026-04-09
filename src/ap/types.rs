//! Minimal ActivityPub / ActivityStreams JSON-LD types.
//!
//! The fediverse uses JSON-LD as "JSON with a @context declaration" — we do not
//! perform full JSON-LD expansion. We deserialize using compacted key names
//! directly. Unknown fields are captured in `extra` where needed.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── @context helpers ─────────────────────────────────────────────────────────

pub const CTX_AS: &str = "https://www.w3.org/ns/activitystreams";
pub const CTX_SEC: &str = "https://w3id.org/security/v1";

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

/// Minimal context for activities we produce (Accept, etc.)
pub fn activity_context() -> Value {
    serde_json::json!([
        "https://www.w3.org/ns/activitystreams",
        "https://w3id.org/security/v1"
    ])
}

// ── Incoming activity envelope ───────────────────────────────────────────────

/// A raw incoming Activity from the inbox. We keep `object` as raw JSON
/// because it can be a URL string, an embedded object, or an array.
#[derive(Debug, Deserialize)]
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

impl StringOrArray {
    pub fn contains(&self, s: &str) -> bool {
        match self {
            StringOrArray::Single(v) => v == s,
            StringOrArray::Array(v) => v.iter().any(|x| x == s),
        }
    }

    pub fn to_vec(&self) -> Vec<&str> {
        match self {
            StringOrArray::Single(s) => vec![s.as_str()],
            StringOrArray::Array(v) => v.iter().map(String::as_str).collect(),
        }
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

#[derive(Debug, Deserialize)]
pub struct Note {
    pub id: String,

    #[serde(rename = "type")]
    pub note_type: String,   // "Note", "Article", etc.

    #[serde(rename = "attributedTo")]
    pub attributed_to: Option<StringOrObject>,

    /// HTML content (the primary representation in AP)
    pub content: Option<String>,

    /// Localised content map; we use the first value if `content` is absent
    #[serde(rename = "contentMap")]
    pub content_map: Option<std::collections::HashMap<String, String>>,

    /// Original source (e.g. Markdown before rendering)
    pub source: Option<NoteSource>,

    pub url: Option<StringOrObject>,

    #[serde(rename = "inReplyTo")]
    pub in_reply_to: Option<StringOrObject>,

    pub to: Option<StringOrArray>,
    pub cc: Option<StringOrArray>,

    pub published: Option<String>,

    pub sensitive: Option<bool>,
    pub summary: Option<String>,

    /// Inline attachments (we note them but don't store)
    pub attachment: Option<Vec<Value>>,

    /// Hashtag / Mention tags
    pub tag: Option<Vec<Value>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NoteSource {
    pub content: String,
    #[serde(rename = "mediaType")]
    pub media_type: String,
}

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

    /// Extract the URL to display (prefer `url`, fall back to `id`).
    pub fn display_url(&self) -> &str {
        self.url
            .as_ref()
            .and_then(|u| u.id())
            .unwrap_or(self.id.as_str())
    }
}

// ── AP Actor ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RemoteActor {
    pub id: String,

    #[serde(rename = "type")]
    pub actor_type: String,  // Person, Service, Application, Group, Organization

    #[serde(rename = "preferredUsername")]
    pub preferred_username: Option<String>,

    pub name: Option<String>,
    pub summary: Option<String>,
    pub url: Option<StringOrObject>,
    pub inbox: Option<String>,

    pub endpoints: Option<ActorEndpoints>,

    #[serde(rename = "publicKey")]
    pub public_key: Option<PublicKeyObject>,

    pub icon: Option<Value>,   // avatar
    pub image: Option<Value>,  // banner
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
