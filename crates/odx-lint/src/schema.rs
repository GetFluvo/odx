//! The database schema an addon declares, as odoo-lint's index reads it,
//! and what changes between two versions of an addon: models, fields,
//! parents (`_inherit`, `_inherits`) and access rights.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

/// A field's type: `Char`, or `Many2one(res.partner)` for a relational
/// field whose comodel is a literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldType {
    /// `Char`, `Many2one`, ...; empty when the index does not know it.
    pub kind: String,
    /// The comodel of a relational field.
    pub comodel: Option<String>,
}

impl fmt::Display for FieldType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = if self.kind.is_empty() {
            "?"
        } else {
            &self.kind
        };
        match &self.comodel {
            Some(comodel) => write!(f, "{kind}({comodel})"),
            None => f.write_str(kind),
        }
    }
}

/// What an addon declares about one model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Model {
    /// Whether the addon creates the model (`_name` not in `_inherit`).
    creates: bool,
    /// `_inherit` parents other than the model itself, and `_inherits`
    /// parents, with which attribute names them.
    parents: BTreeSet<(String, &'static str)>,
    fields: BTreeMap<String, FieldType>,
}

/// The models, fields and access rights of an addon.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schema {
    models: BTreeMap<String, Model>,
    /// Access rights: the model's XML id and the group's, if any.
    access: BTreeSet<(String, Option<String>)>,
}

impl Schema {
    /// The schema of the addon in directory `addon` (named after the
    /// addon), or `None` when it has no readable manifest.
    pub fn of(addon: &Path) -> Option<Self> {
        let index = odoo_lint::index::module_index(addon)?;
        let mut schema = Self::default();
        // odoo-lint indexes only classes with `_name` or `_inherit`.
        let named = index.classes.iter().filter_map(|class| {
            class
                .name
                .iter()
                .chain(&class.inherit)
                .next()
                .map(|name| (class, name))
        });
        for (class, name) in named {
            let model = schema.models.entry(name.clone()).or_default();
            model.creates |= class.name.is_some() && !class.inherit.contains(name);
            model.parents.extend(
                class
                    .inherit
                    .iter()
                    .filter(|parent| *parent != name)
                    .map(|parent| (parent.clone(), "_inherit"))
                    .chain(class.delegates.iter().map(|p| (p.clone(), "_inherits"))),
            );
            model.fields.extend(class.fields.iter().map(|field| {
                (
                    field.name.clone(),
                    FieldType {
                        kind: field.kind.clone(),
                        comodel: field.comodel.clone(),
                    },
                )
            }));
        }
        schema.access = index
            .access
            .iter()
            .map(|access| (access.model.clone(), access.group.clone()))
            .collect();
        Some(schema)
    }

    /// The number of models the addon creates and the fields it declares.
    pub fn size(&self) -> (usize, usize) {
        let created = self.models.values().filter(|model| model.creates).count();
        let fields = self.models.values().map(|model| model.fields.len()).sum();
        (created, fields)
    }
}

/// One difference between two schemas of an addon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaChange {
    /// The addon creates a model it did not.
    ModelAdded {
        /// The model.
        model: String,
    },
    /// The addon no longer creates a model.
    ModelRemoved {
        /// The model.
        model: String,
    },
    /// A field is declared that was not.
    FieldAdded {
        /// The model.
        model: String,
        /// The field.
        field: String,
        /// Its type.
        kind: FieldType,
    },
    /// A field is no longer declared.
    FieldRemoved {
        /// The model.
        model: String,
        /// The field.
        field: String,
        /// Its earlier type.
        kind: FieldType,
    },
    /// A field's type or comodel changed.
    FieldChanged {
        /// The model.
        model: String,
        /// The field.
        field: String,
        /// The earlier type.
        from: FieldType,
        /// The type now.
        to: FieldType,
    },
    /// A model has a new parent.
    ParentAdded {
        /// The model.
        model: String,
        /// The parent model.
        parent: String,
        /// `_inherit` or `_inherits`.
        how: &'static str,
    },
    /// A model lost a parent.
    ParentRemoved {
        /// The model.
        model: String,
        /// The parent model.
        parent: String,
        /// `_inherit` or `_inherits`.
        how: &'static str,
    },
    /// An access right is new.
    AccessAdded {
        /// The model's XML id.
        model: String,
        /// The group's XML id; none for everyone.
        group: Option<String>,
    },
    /// An access right is gone.
    AccessRemoved {
        /// The model's XML id.
        model: String,
        /// The group's XML id; none for everyone.
        group: Option<String>,
    },
}

impl SchemaChange {
    /// `model-added`, `field-changed`, ...
    pub const fn name(&self) -> &'static str {
        match self {
            Self::ModelAdded { .. } => "model-added",
            Self::ModelRemoved { .. } => "model-removed",
            Self::FieldAdded { .. } => "field-added",
            Self::FieldRemoved { .. } => "field-removed",
            Self::FieldChanged { .. } => "field-changed",
            Self::ParentAdded { .. } => "parent-added",
            Self::ParentRemoved { .. } => "parent-removed",
            Self::AccessAdded { .. } => "access-added",
            Self::AccessRemoved { .. } => "access-removed",
        }
    }
}

impl fmt::Display for SchemaChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let group = |group: &Option<String>| group.as_deref().unwrap_or("everyone").to_owned();
        match self {
            Self::ModelAdded { model } => write!(f, "+ model {model}"),
            Self::ModelRemoved { model } => write!(f, "- model {model}"),
            Self::FieldAdded { model, field, kind } => write!(f, "+ field {model}.{field}: {kind}"),
            Self::FieldRemoved { model, field, kind } => {
                write!(f, "- field {model}.{field}: {kind}")
            }
            Self::FieldChanged {
                model,
                field,
                from,
                to,
            } => write!(f, "~ field {model}.{field}: {from} → {to}"),
            Self::ParentAdded { model, parent, how } => write!(f, "+ {how} {model}: {parent}"),
            Self::ParentRemoved { model, parent, how } => write!(f, "- {how} {model}: {parent}"),
            Self::AccessAdded { model, group: g } => write!(f, "+ access {model} ({})", group(g)),
            Self::AccessRemoved { model, group: g } => {
                write!(f, "- access {model} ({})", group(g))
            }
        }
    }
}

/// What changed from `old` to `new`, model by model, then the access
/// rights.
pub fn changes(old: &Schema, new: &Schema) -> Vec<SchemaChange> {
    let mut changes = Vec::new();
    let empty = Model::default();
    let names: BTreeSet<&String> = old.models.keys().chain(new.models.keys()).collect();
    for name in names {
        let before = old.models.get(name).unwrap_or(&empty);
        let after = new.models.get(name).unwrap_or(&empty);
        let model = || name.clone();
        match (before.creates, after.creates) {
            (false, true) => changes.push(SchemaChange::ModelAdded { model: model() }),
            (true, false) => changes.push(SchemaChange::ModelRemoved { model: model() }),
            _ => {}
        }
        for (parent, how) in after.parents.difference(&before.parents) {
            changes.push(SchemaChange::ParentAdded {
                model: model(),
                parent: parent.clone(),
                how,
            });
        }
        for (parent, how) in before.parents.difference(&after.parents) {
            changes.push(SchemaChange::ParentRemoved {
                model: model(),
                parent: parent.clone(),
                how,
            });
        }
        let fields: BTreeSet<&String> = before.fields.keys().chain(after.fields.keys()).collect();
        for field in fields {
            let change = match (before.fields.get(field), after.fields.get(field)) {
                (None, Some(kind)) => SchemaChange::FieldAdded {
                    model: model(),
                    field: field.clone(),
                    kind: kind.clone(),
                },
                (Some(kind), None) => SchemaChange::FieldRemoved {
                    model: model(),
                    field: field.clone(),
                    kind: kind.clone(),
                },
                (Some(from), Some(to)) if from != to => SchemaChange::FieldChanged {
                    model: model(),
                    field: field.clone(),
                    from: from.clone(),
                    to: to.clone(),
                },
                _ => continue,
            };
            changes.push(change);
        }
    }
    for (model, group) in new.access.difference(&old.access) {
        changes.push(SchemaChange::AccessAdded {
            model: model.clone(),
            group: group.clone(),
        });
    }
    for (model, group) in old.access.difference(&new.access) {
        changes.push(SchemaChange::AccessRemoved {
            model: model.clone(),
            group: group.clone(),
        });
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addon(root: &Path, name: &str, python: &str, access: Option<&str>) -> std::path::PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::create_dir_all(dir.join("security")).unwrap();
        let data = if access.is_some() {
            "\"data\": [\"security/ir.model.access.csv\"], "
        } else {
            ""
        };
        std::fs::write(
            dir.join("__manifest__.py"),
            format!("{{\"name\": \"X\", \"version\": \"17.0.1.0.0\", {data}}}\n"),
        )
        .unwrap();
        std::fs::write(dir.join("__init__.py"), "from . import models\n").unwrap();
        std::fs::write(dir.join("models/__init__.py"), "from . import x\n").unwrap();
        std::fs::write(
            dir.join("models/x.py"),
            format!("from odoo import fields, models\n\n{python}"),
        )
        .unwrap();
        if let Some(access) = access {
            std::fs::write(
                dir.join("security/ir.model.access.csv"),
                format!(
                    "id,name,model_id:id,group_id:id,perm_read,perm_write,perm_create,perm_unlink\n{access}"
                ),
            )
            .unwrap();
        }
        dir
    }

    const OLD: &str = "\
class Order(models.Model):
    _inherit = \"pos.order\"

    note = fields.Char()
    partner = fields.Many2one(\"res.partner\")
    gone = fields.Integer()


class Old(models.Model):
    _name = \"pos.old\"

    name = fields.Char()
";

    const NEW: &str = "\
class Order(models.Model):
    _name = \"pos.order\"
    _inherit = [\"pos.order\", \"mail.thread\"]

    note = fields.Char()
    partner = fields.Many2one(\"res.users\")
    added = fields.Text()


class Line(models.Model):
    _name = \"pos.x.line\"
    _inherits = {\"res.partner\": \"partner_id\"}

    name = fields.Char()


class Mixin(models.AbstractModel):
    _inherit = \"pos.old\"
";

    #[test]
    fn models_fields_parents_and_access_are_compared() {
        let tmp = tempfile::tempdir().unwrap();
        let old = Schema::of(&addon(
            &tmp.path().join("old"),
            "pos_x",
            OLD,
            Some("a,a,model_pos_old,,1,1,1,1\nb,b,point_of_sale.model_pos_order,base.group_user,1,0,0,0\n"),
        ))
        .unwrap();
        let new = Schema::of(&addon(
            &tmp.path().join("new"),
            "pos_x",
            NEW,
            Some("c,c,model_pos_x_line,base.group_user,1,1,1,1\nb,b,point_of_sale.model_pos_order,base.group_user,1,0,0,0\n"),
        ))
        .unwrap();
        assert_eq!(old.size(), (1, 4));
        let lines: Vec<String> = changes(&old, &new)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            lines,
            [
                "- model pos.old",
                "- field pos.old.name: Char",
                "+ _inherit pos.order: mail.thread",
                "+ field pos.order.added: Text",
                "- field pos.order.gone: Integer",
                "~ field pos.order.partner: Many2one(res.partner) → Many2one(res.users)",
                "+ model pos.x.line",
                "+ _inherits pos.x.line: res.partner",
                "+ field pos.x.line.name: Char",
                "+ access pos_x.model_pos_x_line (base.group_user)",
                "- access pos_x.model_pos_old (everyone)",
            ]
        );
        assert_eq!(changes(&new, &new), Vec::new());
        assert_eq!(changes(&old, &new)[2].name(), "parent-added");
        assert_eq!(
            changes(&new, &old)[2].to_string(),
            "- _inherit pos.order: mail.thread"
        );
        let back: Vec<&str> = changes(&new, &old).iter().map(SchemaChange::name).collect();
        assert_eq!(
            back,
            [
                "model-added",
                "field-added",
                "parent-removed",
                "field-removed",
                "field-added",
                "field-changed",
                "model-removed",
                "parent-removed",
                "field-removed",
                "access-added",
                "access-removed",
            ]
        );
        // No manifest: no schema.
        assert_eq!(Schema::of(tmp.path()), None);
    }

    #[test]
    fn classes_without_a_model_and_unknown_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let schema = Schema::of(&addon(
            tmp.path(),
            "pos_y",
            "class Plain(models.Model):\n    x = fields.Char()\n",
            None,
        ))
        .unwrap();
        assert_eq!(schema, Schema::default());
        let unknown = FieldType {
            kind: String::new(),
            comodel: None,
        };
        assert_eq!(unknown.to_string(), "?");
    }
}
