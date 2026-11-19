//! Port of `src/main/skill-install.ts` — whether the /rever Claude Code skill is installed.
//!
//! Whether the /rever Claude Code skill is installed globally. The install
//! command itself is shown in the UI for the user to run in their own terminal.

use std::path::{Path, PathBuf};

fn skill_path(home: &Path) -> PathBuf {
    home.join(".claude")
        .join("skills")
        .join("rever")
        .join("SKILL.md")
}

fn skill_installed_in(home: &Path) -> bool {
    skill_path(home).exists()
}

pub fn skill_installed() -> bool {
    std::env::home_dir().is_some_and(|home| skill_installed_in(&home))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn 스킬_파일은_홈_아래_claude_skills_rever에_있다() {
        assert_eq!(
            skill_path(Path::new("/home/u")),
            Path::new("/home/u")
                .join(".claude")
                .join("skills")
                .join("rever")
                .join("SKILL.md")
        );
    }

    #[test]
    fn skill_md가_있어야_설치된_것으로_본다() {
        let home = TempDir::new("skill-install");
        assert!(!skill_installed_in(home.path()));

        // The directory alone is not an install.
        let skill_dir = home.path().join(".claude").join("skills").join("rever");
        assert!(std::fs::create_dir_all(&skill_dir).is_ok());
        assert!(!skill_installed_in(home.path()));

        assert!(std::fs::write(skill_dir.join("SKILL.md"), "# rever\n").is_ok());
        assert!(skill_installed_in(home.path()));
    }

    #[cfg(unix)]
    #[test]
    fn 끊어진_심볼릭_링크는_설치로_보지_않는다() {
        let home = TempDir::new("skill-install-link");
        let skill_dir = home.path().join(".claude").join("skills").join("rever");
        assert!(std::fs::create_dir_all(&skill_dir).is_ok());
        let link = skill_dir.join("SKILL.md");
        assert!(std::os::unix::fs::symlink(home.path().join("gone.md"), &link).is_ok());
        assert!(!skill_installed_in(home.path()));
    }
}
