

use super::bindings::component::uq_process::types::*;

impl PartialEq for ProcessId {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ProcessId::Id(i1), ProcessId::Id(i2)) => i1 == i2,
            (ProcessId::Name(s1), ProcessId::Name(s2)) => s1 == s2,
            _ => false,
        }
    }
}
impl PartialEq<&str> for ProcessId {
    fn eq(&self, other: &&str) -> bool {
        match self {
            ProcessId::Id(_) => false,
            ProcessId::Name(s) => s == other,
        }
    }
}
impl PartialEq<u64> for ProcessId {
    fn eq(&self, other: &u64) -> bool {
        match self {
            ProcessId::Id(i) => i == other,
            ProcessId::Name(_) => false,
        }
    }
}

pub fn parse_message_ipc<T>(json_string: Option<String>) -> anyhow::Result<T>
where
    for<'a> T: serde::Deserialize<'a>
{
    let parsed: T = serde_json::from_str(
        json_string.ok_or(anyhow::anyhow!("json payload empty"))?
                   .as_str()
    )?;
    Ok(parsed)
}
