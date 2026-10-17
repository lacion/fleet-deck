//! The strict JSON readers of `tailscale status --json` and `tailscale whois
//! --json` (moved out of `tailscale.rs` for size; no behavior change).

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::Deserialize;
use serde::de::{self, DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

use super::{Node, Status, TailscaleError, WhoIs, parse_error};
use crate::netscope::{self, AddrClass};

/// A JSON value that refuses a duplicate key at any depth (`serde_json`'s own
/// `Value` keeps the last one).
struct Strict(Value);

impl<'de> de::Deserialize<'de> for Strict {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(StrictVisitor).map(Self)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("a number that is not finite"))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(Strict(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate key `{key}`")));
            }
            let Strict(value) = map.next_value()?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}

/// Parses `bytes` as `T`, and refuses a duplicate key anywhere.
fn parse_strict<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, TailscaleError> {
    let Strict(value) = serde_json::from_slice(bytes).map_err(parse_error)?;
    serde_json::from_value(value).map_err(parse_error)
}

#[derive(Deserialize)]
struct RawStatus {
    #[serde(rename = "BackendState")]
    backend_state: String,
    #[serde(rename = "Self")]
    this: Option<RawSelf>,
    #[serde(rename = "Peer")]
    peer: Option<BTreeMap<String, RawPeer>>,
    #[serde(rename = "User")]
    user: Option<BTreeMap<String, RawUser>>,
    #[serde(rename = "CurrentTailnet")]
    current_tailnet: Option<RawTailnet>,
    #[serde(rename = "MagicDNSSuffix")]
    magic_dns_suffix: Option<String>,
}

#[derive(Deserialize)]
struct RawSelf {
    #[serde(rename = "HostName")]
    host_name: Option<String>,
    #[serde(rename = "DNSName")]
    dns_name: String,
    #[serde(rename = "TailscaleIPs")]
    ips: Vec<String>,
    #[serde(rename = "UserID")]
    user_id: Option<Number>,
    #[serde(rename = "Tags")]
    tags: Option<Vec<String>>,
    #[serde(rename = "OS")]
    os: Option<String>,
}

#[derive(Deserialize)]
struct RawPeer {
    #[serde(rename = "HostName")]
    host_name: String,
    #[serde(rename = "DNSName")]
    dns_name: Option<String>,
    #[serde(rename = "TailscaleIPs")]
    ips: Vec<String>,
    #[serde(rename = "UserID")]
    user_id: Option<Number>,
    #[serde(rename = "Tags")]
    tags: Option<Vec<String>>,
    #[serde(rename = "OS")]
    os: Option<String>,
    #[serde(rename = "Online")]
    online: Option<bool>,
}

#[derive(Deserialize)]
struct RawUser {
    #[serde(rename = "LoginName")]
    login_name: Option<String>,
}

#[derive(Deserialize)]
struct RawTailnet {
    #[serde(rename = "MagicDNSSuffix")]
    magic_dns_suffix: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
}

/// The tailnet addresses of a node: valid, and inside the Tailscale range.
fn tailnet_ips(texts: &[String]) -> Result<Vec<IpAddr>, TailscaleError> {
    texts
        .iter()
        .map(|text| {
            let ip: IpAddr = text
                .parse()
                .map_err(|_| parse_error("TailscaleIPs has an address that is not valid"))?;
            if netscope::classify(ip) == AddrClass::Tailnet {
                Ok(ip)
            } else {
                Err(parse_error(
                    "TailscaleIPs has an address outside the tailnet",
                ))
            }
        })
        .collect()
}

fn login_of(users: &BTreeMap<String, RawUser>, user_id: Option<&Number>) -> Option<String> {
    user_id
        .and_then(|id| users.get(&id.to_string()))
        .and_then(|u| u.login_name.clone())
        .filter(|l| !l.is_empty())
}

fn dns_name(raw: Option<String>) -> String {
    raw.unwrap_or_default().trim_end_matches('.').to_owned()
}

/// Parses the document of `tailscale status --json`.
///
/// # Errors
/// [`TailscaleError::NotRunning`] unless `BackendState` is `Running`;
/// [`TailscaleError::Parse`] for a duplicate key, a missing required field or
/// an address outside the Tailscale range.
pub fn parse_status(bytes: &[u8]) -> Result<Status, TailscaleError> {
    let raw: RawStatus = parse_strict(bytes)?;
    if raw.backend_state != "Running" {
        return Err(TailscaleError::NotRunning);
    }
    let this = raw
        .this
        .ok_or_else(|| parse_error("missing field `Self`"))?;
    let users = raw.user.unwrap_or_default();
    let ips = tailnet_ips(&this.ips)?;
    if ips.is_empty() {
        return Err(parse_error("Self has no TailscaleIPs"));
    }
    let me = Node {
        host_name: this.host_name.unwrap_or_default(),
        dns_name: dns_name(Some(this.dns_name)),
        ips,
        os: this.os,
        login: login_of(&users, this.user_id.as_ref()),
        tags: this.tags.unwrap_or_default(),
        online: true,
    };
    let mut peers = Vec::new();
    for peer in raw.peer.unwrap_or_default().into_values() {
        peers.push(Node {
            host_name: peer.host_name,
            dns_name: dns_name(peer.dns_name),
            ips: tailnet_ips(&peer.ips)?,
            os: peer.os,
            login: login_of(&users, peer.user_id.as_ref()),
            tags: peer.tags.unwrap_or_default(),
            online: peer.online.unwrap_or(false),
        });
    }
    let (tailnet_suffix, current_tailnet) = match raw.current_tailnet {
        Some(t) => (t.magic_dns_suffix, t.name.filter(|n| !n.is_empty())),
        None => (None, None),
    };
    let magic_dns_suffix = tailnet_suffix
        .or(raw.magic_dns_suffix)
        .filter(|s| !s.is_empty());
    Ok(Status {
        this: me,
        peers,
        magic_dns_suffix,
        current_tailnet,
    })
}

#[derive(Deserialize)]
struct RawWhoIs {
    #[serde(rename = "Node")]
    node: RawWhoNode,
    #[serde(rename = "UserProfile")]
    profile: Option<RawProfile>,
}

#[derive(Deserialize)]
struct RawWhoNode {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Addresses")]
    addresses: Option<Vec<String>>,
    #[serde(rename = "Tags")]
    tags: Option<Vec<String>>,
    #[serde(rename = "Sharer")]
    sharer: Option<Value>,
}

#[derive(Deserialize)]
struct RawProfile {
    #[serde(rename = "LoginName")]
    login_name: Option<String>,
}

/// Parses the document of `tailscale whois --json <ip>`. An address may have
/// a prefix length (`100.64.0.1/32`).
///
/// # Errors
/// [`TailscaleError::Parse`] for a duplicate key, a missing `Node.Name` or an
/// address that is not valid.
pub fn parse_whois(bytes: &[u8]) -> Result<WhoIs, TailscaleError> {
    let raw: RawWhoIs = parse_strict(bytes)?;
    let addresses = raw
        .node
        .addresses
        .unwrap_or_default()
        .iter()
        .map(|a| {
            a.split('/')
                .next()
                .unwrap_or_default()
                .parse()
                .map_err(|_| parse_error("Addresses has an address that is not valid"))
        })
        .collect::<Result<Vec<IpAddr>, _>>()?;
    Ok(WhoIs {
        name: raw.node.name.trim_end_matches('.').to_owned(),
        addresses,
        tags: raw.node.tags.unwrap_or_default(),
        login: raw
            .profile
            .and_then(|p| p.login_name)
            .filter(|l| !l.is_empty()),
        shared: raw.node.sharer.filter(|s| !s.is_null()).map(|_| true),
    })
}
