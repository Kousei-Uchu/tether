//! Per-device permissions. A host keeps one [`PermissionSet`] per paired device and
//! checks it before doing anything on that device's behalf.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use tether_proto::Permission;

/// Small bit set over [`Permission`]. Serializes as a list of lowercase names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Vec<Permission>", into = "Vec<Permission>")]
pub struct PermissionSet(u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("permission denied: this device may not use `{0}`")]
pub struct PermissionDenied(pub Permission);

const fn bit(p: Permission) -> u8 {
    1 << (p as u8)
}

impl PermissionSet {
    pub const fn none() -> Self {
        Self(0)
    }

    pub fn all() -> Self {
        Permission::ALL.iter().fold(Self::none(), |s, p| s.with(*p))
    }

    /// What a newly paired device gets unless the host says otherwise: look, don't touch.
    pub const fn default_grant() -> Self {
        Self(bit(Permission::View))
    }

    pub const fn with(self, p: Permission) -> Self {
        Self(self.0 | bit(p))
    }

    pub const fn without(self, p: Permission) -> Self {
        Self(self.0 & !bit(p))
    }

    pub const fn contains(self, p: Permission) -> bool {
        self.0 & bit(p) != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn minus(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Members in a fixed order.
    pub fn to_vec(self) -> Vec<Permission> {
        Permission::ALL
            .iter()
            .copied()
            .filter(|p| self.contains(*p))
            .collect()
    }

    pub fn require(self, p: Permission) -> Result<(), PermissionDenied> {
        if self.contains(p) {
            Ok(())
        } else {
            Err(PermissionDenied(p))
        }
    }
}

impl Default for PermissionSet {
    fn default() -> Self {
        Self::none()
    }
}

impl From<Vec<Permission>> for PermissionSet {
    fn from(list: Vec<Permission>) -> Self {
        list.into_iter().fold(Self::none(), Self::with)
    }
}

impl From<PermissionSet> for Vec<Permission> {
    fn from(set: PermissionSet) -> Self {
        set.to_vec()
    }
}

impl fmt::Display for PermissionSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("none");
        }
        let names: Vec<&str> = self.to_vec().into_iter().map(Permission::name).collect();
        f.write_str(&names.join(","))
    }
}

impl FromStr for PermissionSet {
    type Err = String;

    /// Accepts `view,input`, `all` and `none`. Case and spaces are ignored.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut set = Self::none();
        for raw in s.split(',') {
            let word = raw.trim().to_lowercase();
            match word.as_str() {
                "" => {}
                "none" => {}
                "all" => set = set.union(Self::all()),
                other => {
                    let p = Permission::ALL
                        .iter()
                        .find(|p| p.name() == other)
                        .ok_or_else(|| {
                            format!(
                                "unknown permission `{other}` (use: {}, all, none)",
                                Permission::ALL.map(Permission::name).join(", ")
                            )
                        })?;
                    set = set.with(*p);
                }
            }
        }
        Ok(set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_operations() {
        let s = PermissionSet::none()
            .with(Permission::View)
            .with(Permission::Power);
        assert!(s.contains(Permission::View) && s.contains(Permission::Power));
        assert!(!s.contains(Permission::Input));
        assert_eq!(
            s.without(Permission::View).to_vec(),
            vec![Permission::Power]
        );
        assert_eq!(PermissionSet::all().to_vec().len(), Permission::ALL.len());
        assert!(s.require(Permission::Input).is_err());
        assert!(s.require(Permission::Power).is_ok());
    }

    #[test]
    fn parse_and_display_roundtrip() {
        let s: PermissionSet = "View, input ,POWER".parse().unwrap();
        assert_eq!(s.to_string(), "view,input,power");
        assert_eq!(
            "all".parse::<PermissionSet>().unwrap(),
            PermissionSet::all()
        );
        assert_eq!(
            "none".parse::<PermissionSet>().unwrap(),
            PermissionSet::none()
        );
        assert_eq!(PermissionSet::none().to_string(), "none");
        assert!("view,fly".parse::<PermissionSet>().is_err());
    }

    #[test]
    fn json_is_a_list_of_names() {
        let s = PermissionSet::default_grant().with(Permission::Files);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"["view","files"]"#);
        assert_eq!(serde_json::from_str::<PermissionSet>(&json).unwrap(), s);
    }
}
