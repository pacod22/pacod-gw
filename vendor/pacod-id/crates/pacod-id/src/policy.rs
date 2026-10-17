//! Password policy: a realm's minimum (12 unless the realm sets another), 12 for
//! anyone holding a role wherever they set it, at most 128, no composition rules, and none of the
//! most common passwords (data/common-passwords.txt, see data/NOTICE).

use std::collections::HashSet;
use std::sync::LazyLock;

use crate::config::Realm;
use crate::store::{Account, Store, StoreError};

pub const MAX_PASSWORD_LENGTH: usize = 128;

static COMMON: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| include_str!("../data/common-passwords.txt").lines().filter(|l| !l.is_empty()).collect());

/// Why a password is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordProblem {
    TooShort { min: usize },
    TooLong,
    Common,
}

impl PasswordProblem {
    pub fn code(self) -> &'static str {
        match self {
            PasswordProblem::TooShort { .. } => "password_too_short",
            PasswordProblem::TooLong => "password_too_long",
            PasswordProblem::Common => "password_too_common",
        }
    }
}

/// Characters as a person counts them.
pub fn length(password: &str) -> usize {
    password.chars().count()
}

pub fn check(password: &str, min: usize) -> Result<(), PasswordProblem> {
    let n = length(password);
    if n < min {
        return Err(PasswordProblem::TooShort { min });
    }
    if n > MAX_PASSWORD_LENGTH {
        return Err(PasswordProblem::TooLong);
    }
    if COMMON.contains(password.to_lowercase().as_str()) {
        return Err(PasswordProblem::Common);
    }
    Ok(())
}

/// The minimum for an account setting its password in `realm`: the realm's, or the role
/// holders' when the account holds any role (a password set in one realm also opens the others).
pub async fn min_length(store: &Store, realm: &Realm, account: Option<&Account>) -> Result<usize, StoreError> {
    match account {
        Some(a) if store.account_holds_roles(&a.id).await? => {
            Ok(realm.role_holder_password_min_length.max(realm.password_min_length))
        }
        _ => Ok(realm.password_min_length),
    }
}

/// The length below which an account must choose a new password when it signs in: only for
/// role holders (existing short passwords of other accounts keep working).
pub async fn forced_min_length(store: &Store, realm: &Realm, account: &Account) -> Result<Option<usize>, StoreError> {
    Ok(store
        .account_holds_roles(&account.id)
        .await?
        .then(|| realm.role_holder_password_min_length.max(realm.password_min_length)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_and_blocklist() {
        assert_eq!(check("short", 8), Err(PasswordProblem::TooShort { min: 8 }));
        assert_eq!(check("Password123", 8), Err(PasswordProblem::Common), "case does not hide a common password");
        assert_eq!(check("qwertyuiop", 8), Err(PasswordProblem::Common));
        assert_eq!(check(&"x".repeat(129), 8), Err(PasswordProblem::TooLong));
        assert_eq!(check("correct horse battery staple", 12), Ok(()));
        // Characters, not bytes: eight Cyrillic letters are eight.
        assert_eq!(check("пароль12", 8), Ok(()));
        assert!(COMMON.len() > 40_000);
    }
}
