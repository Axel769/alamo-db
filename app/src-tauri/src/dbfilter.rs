//! Filter on the databases of a profile (DBeaver "Databases / Catalogs"
//! model; on MySQL/MariaDB the schemas, on PostgreSQL the server's
//! databases, §3.3): the same server split into several profiles, e.g.
//! `*_test*` and `*_demo*` shown only in the test profile and hidden in the
//! production one.
//!
//! It is a **display** filter: it hides the names from the tree, the
//! connection page and the database picker. It is not a permission — from
//! SQL everything the server user can see is still reachable.
//!
//! System schemas do not go through the filter: the UI's "show system"
//! already governs them.

/// Masks already split. Syntax: `*` or `%` = any sequence, the rest is
/// literal (including `_`: in database names it is almost always a
/// separator, not a wildcard), case-insensitive.
#[derive(Debug, Clone, Default)]
pub struct DbFilter {
    include: Vec<String>,
    exclude: Vec<String>,
}

impl DbFilter {
    /// From the profile fields: masks separated by comma or newline.
    pub fn new(include: &str, exclude: &str) -> Self {
        DbFilter {
            include: masks(include),
            exclude: masks(exclude),
        }
    }

    /// Visible = (no "show only" or one of them matches) and no "hide"
    /// matches.
    pub fn shows(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        (self.include.is_empty() || self.include.iter().any(|m| glob(m, &name)))
            && !self.exclude.iter().any(|m| glob(m, &name))
    }
}

fn masks(s: &str) -> Vec<String> {
    s.split([',', '\n'])
        .map(|m| m.trim().to_lowercase())
        .filter(|m| !m.is_empty())
        .collect()
}

/// Glob with the single "any sequence" wildcard, without exponential
/// backtracking: each piece between wildcards is searched starting from
/// where the previous one ended.
fn glob(mask: &str, name: &str) -> bool {
    let parts: Vec<&str> = mask.split(['*', '%']).collect();
    if parts.len() == 1 {
        return mask == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if name.len() < first.len() + last.len() || !name.starts_with(first) || !name.ends_with(last) {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_shows_everything() {
        let f = DbFilter::new("", " , \n");
        assert!(f.shows("erp"));
        assert!(f.shows("erp_test"));
    }

    #[test]
    fn prod_test_split() {
        let test = DbFilter::new("*_test*, *_demo*", "");
        let prod = DbFilter::new("", "*_test*\n*_demo*");
        for (name, is_test) in [
            ("erp", false),
            ("erp_test", true),
            ("ERP_TEST_2", true),
            ("customer_demo", true),
            ("customer_latest", false),
            ("testdb", false),
        ] {
            assert_eq!(test.shows(name), is_test, "{name}");
            assert_eq!(prod.shows(name), !is_test, "{name}");
        }
    }

    #[test]
    fn masks_match() {
        assert!(glob("abc", "abc"));
        assert!(!glob("abc", "abcd"));
        assert!(glob("a*", "a"));
        assert!(glob("*c", "abc"));
        assert!(glob("a%c", "abbc"));
        assert!(glob("a*b*c", "axxbyyc"));
        assert!(!glob("a*b*c", "axxcyyb"));
        assert!(!glob("ab*ba", "aba"));
        assert!(glob("*", ""));
        assert!(!glob("a_b", "axb"));
    }
}
