//! Assembly commands: `set_assembly`, `remove_assembly`, `rename_assembly`.
//! Nothing refers to an assembly (placed copies are independent), so these
//! only touch `assemblies/<slug>.json`.

use super::{Result, noop};
use crate::assembly::Assembly;
use crate::command::{Applied, Change, Command, CommandError, DefKind};
use crate::id::Slug;
use crate::model::Project;

impl Project {
    pub(super) fn set_assembly(&mut self, assembly: Assembly) -> Result<Applied> {
        let slug = assembly.slug.clone();
        let changes = vec![Change::Def {
            kind: DefKind::Assembly,
            slug: slug.clone(),
        }];
        let inverse = match self.assemblies.iter().position(|a| a.slug == slug) {
            Some(k) if self.assemblies[k] == assembly => return Ok(noop()),
            Some(k) => Command::SetAssembly {
                assembly: std::mem::replace(&mut self.assemblies[k], assembly),
            },
            None => {
                let k = self.assemblies.partition_point(|a| a.slug < slug);
                self.assemblies.insert(k, assembly);
                Command::RemoveAssembly { slug }
            }
        };
        Ok(Applied { inverse, changes })
    }

    pub(super) fn remove_assembly(&mut self, slug: Slug) -> Result<Applied> {
        let k = self
            .assemblies
            .iter()
            .position(|a| a.slug == slug)
            .ok_or_else(|| CommandError::UnknownAssembly(slug.clone()))?;
        let old = self.assemblies.remove(k);
        Ok(Applied {
            inverse: Command::SetAssembly { assembly: old },
            changes: vec![Change::Def {
                kind: DefKind::Assembly,
                slug,
            }],
        })
    }

    pub(super) fn rename_assembly(&mut self, slug: Slug, new_slug: Slug) -> Result<Applied> {
        let k = self
            .assemblies
            .iter()
            .position(|a| a.slug == slug)
            .ok_or_else(|| CommandError::UnknownAssembly(slug.clone()))?;
        if slug == new_slug {
            return Ok(noop());
        }
        if self.assembly(new_slug.as_str()).is_some() {
            return Err(CommandError::DuplicateDef {
                kind: DefKind::Assembly,
                slug: new_slug,
            });
        }
        let mut a = self.assemblies.remove(k);
        a.slug = new_slug.clone();
        let k = self.assemblies.partition_point(|x| x.slug < new_slug);
        self.assemblies.insert(k, a);
        Ok(Applied {
            inverse: Command::RenameAssembly {
                slug: new_slug.clone(),
                new_slug: slug.clone(),
            },
            changes: vec![Change::DefRenamed {
                kind: DefKind::Assembly,
                from: slug,
                to: new_slug,
            }],
        })
    }
}
