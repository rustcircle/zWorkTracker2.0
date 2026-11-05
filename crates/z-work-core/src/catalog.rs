//! Tags and task templates.

use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::error::CoreError;
use crate::machine::{Ctx, TaskInput};
use crate::model::{Shared, Stamp, Tag, Template};
use crate::subtasks;
use crate::time::Ts;
use crate::validate::{check_description, clean_color, clean_text};

const MAX_TAG: usize = 40;
const MAX_TEMPLATE: usize = 120;

/// Starter tags, with fixed ids and an old stamp, so devices agree and any edit wins.
const SEEDS: [(&str, &str, &str); 4] = [
    ("tag-concept", "Concept", "#7c3aed"),
    ("tag-revision", "Revision", "#0f766e"),
    ("tag-client-call", "Client call", "#c2410c"),
    ("tag-admin", "Admin", "#475569"),
];

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagInput {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub color: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateInput {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub description: Value,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Shared {
    /// Puts data written elsewhere into the form this version uses. Pure and deterministic, so devices
    /// agree without a new stamp: subtask ids for lists older than them, and canonical colours.
    pub fn normalize(&mut self) {
        for t in self.tasks.values_mut() {
            // What ids would take past the limit could never be saved, so it stays as it is.
            if let Some(d) = crate::subtasks::stable_ids(&t.id, &t.description)
                .filter(|d| crate::validate::check_description(d).is_ok())
            {
                t.description = d;
            }
        }
        for p in self.projects.values_mut() {
            p.color.make_ascii_lowercase();
        }
        for t in self.tags.values_mut() {
            t.color.make_ascii_lowercase();
        }
    }

    /// Adds the starter tags to a store that has never had any. A deleted tag stays deleted.
    pub fn seed_tags(&mut self) {
        if !self.tags.is_empty() {
            return;
        }
        for (id, name, color) in SEEDS {
            self.tags.insert(
                id.to_owned(),
                Tag {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    color: color.to_owned(),
                    deleted: false,
                    stamp: Stamp {
                        at: Ts::EARLIEST,
                        by: String::new(),
                    },
                },
            );
        }
    }

    pub fn check_tag_ids(&self, ids: &[String]) -> Result<(), CoreError> {
        if ids.len() > 32 {
            return Err(CoreError::Validation("too many tags".into()));
        }
        for id in ids {
            if !self.tags.get(id).is_some_and(|t| !t.deleted) {
                return Err(CoreError::NotFound("tag"));
            }
        }
        Ok(())
    }

    pub fn save_tag(&mut self, ctx: &Ctx<'_>, input: &TagInput) -> Result<String, CoreError> {
        let name = clean_text(&input.name, "Tag name", MAX_TAG)?;
        let color = clean_color(&input.color)?;
        let clash = self.tags.values().any(|t| {
            !t.deleted
                && Some(t.id.as_str()) != input.id.as_deref()
                && t.name.to_lowercase() == name.to_lowercase()
        });
        if clash {
            return Err(CoreError::Conflict(format!(
                "a tag named \"{name}\" already exists"
            )));
        }
        let (id, stamp) = match input.id.as_deref() {
            Some(id) => {
                let old = self
                    .tags
                    .get(id)
                    .filter(|t| !t.deleted)
                    .ok_or(CoreError::NotFound("tag"))?;
                (id.to_owned(), ctx.next(&old.stamp))
            }
            None => (Uuid::new_v4().to_string(), ctx.stamp()),
        };
        self.tags.insert(
            id.clone(),
            Tag {
                id: id.clone(),
                name,
                color,
                deleted: false,
                stamp,
            },
        );
        Ok(id)
    }

    pub fn delete_tag(&mut self, ctx: &Ctx<'_>, id: &str) -> Result<(), CoreError> {
        let tag = self.tags.get_mut(id).ok_or(CoreError::NotFound("tag"))?;
        tag.deleted = true;
        tag.stamp = ctx.next(&tag.stamp);
        Ok(())
    }

    pub fn save_template(
        &mut self,
        ctx: &Ctx<'_>,
        input: &TemplateInput,
    ) -> Result<String, CoreError> {
        let name = clean_text(&input.name, "Template name", MAX_TEMPLATE)?;
        check_description(&input.description)?;
        self.check_tag_ids(&input.tags)?;
        if let Some(p) = &input.project_id {
            self.live_project(p)?;
        }
        let (id, stamp) = match input.id.as_deref() {
            Some(id) => {
                let old = self
                    .templates
                    .get(id)
                    .filter(|t| !t.deleted)
                    .ok_or(CoreError::NotFound("template"))?;
                (id.to_owned(), ctx.next(&old.stamp))
            }
            None => (Uuid::new_v4().to_string(), ctx.stamp()),
        };
        self.templates.insert(
            id.clone(),
            Template {
                id: id.clone(),
                name,
                project_id: input.project_id.clone(),
                description: subtasks::reset(&input.description),
                tags: input.tags.clone(),
                deleted: false,
                stamp,
            },
        );
        Ok(id)
    }

    pub fn delete_template(&mut self, ctx: &Ctx<'_>, id: &str) -> Result<(), CoreError> {
        let t = self
            .templates
            .get_mut(id)
            .ok_or(CoreError::NotFound("template"))?;
        t.deleted = true;
        t.stamp = ctx.next(&t.stamp);
        Ok(())
    }

    /// A new draft from a template. `project_id` fills in a template that has none.
    pub fn task_from_template(
        &mut self,
        ctx: &Ctx<'_>,
        id: &str,
        project_id: Option<&str>,
    ) -> Result<String, CoreError> {
        let t = self
            .templates
            .get(id)
            .filter(|t| !t.deleted)
            .ok_or(CoreError::NotFound("template"))?;
        let project = project_id
            .map(str::to_owned)
            .or_else(|| t.project_id.clone())
            .ok_or_else(|| CoreError::Validation("Project is required".into()))?;
        let tags = t
            .tags
            .iter()
            .filter(|tag| self.tags.get(*tag).is_some_and(|x| !x.deleted))
            .cloned()
            .collect();
        let input = TaskInput {
            name: t.name.clone(),
            project_id: project,
            description: subtasks::reset(&t.description),
            continued_from: None,
            tags,
        };
        self.create_task(ctx, &input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::tests::{ctx, project_input};
    use serde_json::json;

    fn world() -> (Shared, String) {
        let mut s = Shared::default();
        s.seed_tags();
        let p = s
            .create_project(&ctx("2026-01-01T08:00:00Z"), &project_input("A"))
            .unwrap();
        (s, p)
    }

    #[test]
    fn starter_tags_appear_once_and_a_deleted_one_stays_deleted() {
        let (mut s, _) = world();
        assert_eq!(s.tags.len(), 4);
        s.delete_tag(&ctx("2026-01-01T09:00:00Z"), "tag-admin")
            .unwrap();
        s.seed_tags();
        assert!(s.tags["tag-admin"].deleted);
    }

    #[test]
    fn tag_names_are_unique_ignoring_case() {
        let (mut s, _) = world();
        let c = ctx("2026-01-01T09:00:00Z");
        let input = TagInput {
            id: None,
            name: "concept".into(),
            color: "#112233".into(),
        };
        assert!(matches!(
            s.save_tag(&c, &input),
            Err(CoreError::Conflict(_))
        ));
        let renamed = TagInput {
            id: Some("tag-concept".into()),
            name: "CONCEPT".into(),
            color: "#112233".into(),
        };
        s.save_tag(&c, &renamed).unwrap();
        assert_eq!(s.tags["tag-concept"].name, "CONCEPT");
    }

    #[test]
    fn a_template_makes_an_independent_draft_with_unchecked_subtasks() {
        let (mut s, p) = world();
        let c = ctx("2026-01-01T09:00:00Z");
        let doc = json!({"type":"doc","content":[{"type":"taskList","content":[
            {"type":"taskItem","attrs":{"checked":true,"subtaskId":"x"},"content":[{"type":"paragraph","content":[{"type":"text","text":"Sketch"}]}]}]}]});
        let id = s
            .save_template(
                &c,
                &TemplateInput {
                    id: None,
                    name: "Logo job".into(),
                    project_id: Some(p.clone()),
                    description: doc,
                    tags: vec!["tag-concept".into()],
                },
            )
            .unwrap();
        let a = s.task_from_template(&c, &id, None).unwrap();
        let b = s.task_from_template(&c, &id, None).unwrap();
        assert_ne!(a, b);
        let items = subtasks::items(&s.tasks[&a].description);
        assert_eq!(
            (items[0].text.as_str(), items[0].checked),
            ("Sketch", false)
        );
        assert_ne!(items[0].id, subtasks::items(&s.tasks[&b].description)[0].id);
        assert_eq!(s.tasks[&a].tags, vec!["tag-concept".to_owned()]);
    }

    #[test]
    fn a_template_without_a_project_needs_one_to_be_used() {
        let (mut s, p) = world();
        let c = ctx("2026-01-01T09:00:00Z");
        let id = s
            .save_template(
                &c,
                &TemplateInput {
                    id: None,
                    name: "T".into(),
                    project_id: None,
                    description: Value::Null,
                    tags: vec![],
                },
            )
            .unwrap();
        assert!(s.task_from_template(&c, &id, None).is_err());
        assert!(s.task_from_template(&c, &id, Some(&p)).is_ok());
    }
}
