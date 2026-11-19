//! RBAC guard — permission checks only, never role checks (portal.md D5).
//!
//! Handlers call [`check`] with a permission string from the §6.10.1 matrix.
//! The permission map comes from the scheduler (role JSON); [`permissions_for_role`]
//! mirrors the seed and is used by tests and the fake backend.

use std::collections::BTreeMap;

use crate::auth::AuthContext;
use crate::error::PortalError;

/// The 14 permissions of §6.10.1, in matrix order.
pub const PERMISSIONS: [&str; 14] = [
    "library.read",
    "library.upload",
    "library.delete",
    "queue.write.own",
    "queue.write.any",
    "show.write",
    "broadcast.write",
    "control.read",
    "control.write",
    "announcement.write",
    "tape.publish",
    "caster.manage",
    "settings.manage",
    "event_log.read",
];

/// The permission matrix (§6.10.1) as `(permission, caster, operator, admin)`.
const MATRIX: [(&str, bool, bool, bool); 14] = [
    ("library.read", true, true, true),
    ("library.upload", true, true, true),
    ("library.delete", false, true, true),
    ("queue.write.own", true, true, true),
    ("queue.write.any", false, true, true),
    ("show.write", true, true, true),
    ("broadcast.write", true, true, true),
    ("control.read", true, true, true),
    ("control.write", false, true, true),
    ("announcement.write", true, true, true),
    ("tape.publish", true, true, true),
    ("caster.manage", false, true, true),
    ("settings.manage", false, false, true),
    ("event_log.read", false, true, true),
];

/// Permission map for a role name, mirroring the seeded `roles.permissions`.
pub fn permissions_for_role(role: &str) -> BTreeMap<String, bool> {
    let index = match role {
        "caster" => 0,
        "operator" => 1,
        "admin" => 2,
        _ => 0, // unknown role → least privilege (caster)
    };
    MATRIX
        .iter()
        .map(|(permission, caster, operator, admin)| {
            let allowed = [*caster, *operator, *admin][index];
            (permission.to_string(), allowed)
        })
        .collect()
}

/// Returns `Ok(())` when `ctx` holds `permission`, else `403`.
pub fn check(ctx: &AuthContext, permission: &str) -> Result<(), PortalError> {
    if ctx.has(permission) {
        Ok(())
    } else {
        Err(PortalError::Forbidden(format!(
            "missing permission {permission}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn ctx(role: &str) -> AuthContext {
        AuthContext {
            session_id: "s".into(),
            caster_id: "c".into(),
            username: "u".into(),
            display_name: "U".into(),
            role: role.into(),
            permissions: permissions_for_role(role),
            csrf_token: "t".into(),
        }
    }

    #[test]
    fn matrix_matches_spec() {
        let caster = permissions_for_role("caster");
        assert_eq!(caster.get("library.read"), Some(&true));
        assert_eq!(caster.get("control.write"), Some(&false));
        assert_eq!(caster.get("caster.manage"), Some(&false));
        assert_eq!(caster.get("settings.manage"), Some(&false));

        let operator = permissions_for_role("operator");
        assert_eq!(operator.get("control.write"), Some(&true));
        assert_eq!(operator.get("caster.manage"), Some(&true));
        assert_eq!(operator.get("settings.manage"), Some(&false));

        let admin = permissions_for_role("admin");
        assert_eq!(admin.get("settings.manage"), Some(&true));
        assert_eq!(admin.len(), 14);
    }

    #[test]
    fn caster_is_forbidden_control_write_and_admin() {
        let caster = ctx("caster");
        assert!(check(&caster, "library.read").is_ok());
        assert!(check(&caster, "control.write").is_err());
        assert!(check(&caster, "caster.manage").is_err());
        assert!(check(&caster, "settings.manage").is_err());

        let operator = ctx("operator");
        assert!(check(&operator, "control.write").is_ok());
        assert!(check(&operator, "caster.manage").is_ok());
        assert!(check(&operator, "settings.manage").is_err());
    }

    #[test]
    fn unknown_permission_is_denied() {
        let admin = ctx("admin");
        assert!(check(&admin, "does.not.exist").is_err());
    }

    #[test]
    fn empty_permission_map_denies_everything() {
        let ctx = AuthContext {
            session_id: "s".into(),
            caster_id: "c".into(),
            username: "u".into(),
            display_name: "U".into(),
            role: "caster".into(),
            permissions: BTreeMap::new(),
            csrf_token: "t".into(),
        };
        assert!(check(&ctx, "library.read").is_err());
    }
}
