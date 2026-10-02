//! What a desktop file actually contains: tables, columns, other objects.

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde::Serialize;

use super::catalog::{self, Disposition, Presence};

/// The schema of an opened desktop file.
#[derive(Debug, Clone, Serialize)]
pub struct LegacySchema {
    /// `PRAGMA user_version`: the desktop's data-repair level (3 today).
    pub user_version: i64,
    /// `wal` or `rollback`, from the file header (the desktop uses WAL).
    pub journal_mode: String,
    /// Every table, SQLite-internal ones included, with its columns in
    /// declaration order.
    pub tables: BTreeMap<String, Vec<ColumnInfo>>,
    /// Views and triggers. The desktop creates none.
    pub other_objects: Vec<SchemaObject>,
}

/// A column as declared in the file.
#[derive(Debug, Clone, Serialize)]
pub struct ColumnInfo {
    pub name: String,
    pub declared_type: String,
    pub not_null: bool,
    pub default_sql: Option<String>,
    pub primary_key: bool,
}

/// A view or trigger.
#[derive(Debug, Clone, Serialize)]
pub struct SchemaObject {
    pub kind: String,
    pub name: String,
    pub table: String,
}

impl LegacySchema {
    pub(crate) fn introspect(
        conn: &Connection,
        journal_mode: String,
    ) -> rusqlite::Result<LegacySchema> {
        let user_version = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

        let mut tables = BTreeMap::new();
        let mut other_objects = Vec::new();
        let mut objects = conn.prepare(
            "SELECT type, name, tbl_name FROM sqlite_master \
             WHERE type IN ('table', 'view', 'trigger') ORDER BY type, name",
        )?;
        let rows = objects.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut table_names = Vec::new();
        for row in rows {
            let (kind, name, table) = row?;
            if kind == "table" {
                table_names.push(name);
            } else {
                other_objects.push(SchemaObject { kind, name, table });
            }
        }
        let mut info = conn.prepare(
            "SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info(?1) ORDER BY cid",
        )?;
        for name in table_names {
            let columns = info
                .query_map([&name], |r| {
                    Ok(ColumnInfo {
                        name: r.get(0)?,
                        declared_type: r.get(1)?,
                        not_null: r.get::<_, i64>(2)? != 0,
                        default_sql: r.get(3)?,
                        primary_key: r.get::<_, i64>(4)? != 0,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            tables.insert(name, columns);
        }
        Ok(LegacySchema {
            user_version,
            journal_mode,
            tables,
            other_objects,
        })
    }

    pub fn has_table(&self, table: &str) -> bool {
        self.tables.contains_key(table)
    }

    pub fn columns(&self, table: &str) -> Option<&[ColumnInfo]> {
        self.tables.get(table).map(Vec::as_slice)
    }

    pub fn has_column(&self, table: &str, column: &str) -> bool {
        self.columns(table)
            .is_some_and(|cols| cols.iter().any(|c| c.name == column))
    }

    /// Compares the file with the catalog: every table and column of the file
    /// must be known (mapped or explicitly dropped).
    pub fn coverage(&self) -> Coverage {
        let mut columns = Vec::new();
        let mut tables = Vec::new();
        for spec in catalog::TABLES {
            let present = self.columns(spec.name);
            tables.push(TableCoverage {
                table: spec.name.to_owned(),
                status: match present {
                    Some(_) => TableStatus::Known,
                    None if spec.required => TableStatus::AbsentRequired,
                    None => TableStatus::AbsentOptional,
                },
                disposition: Some(spec.disposition),
            });
            let Some(present) = present else { continue };
            for c in spec.columns {
                let found = present.iter().any(|p| p.name == c.name);
                let status = match (found, c.presence) {
                    (true, _) => ColumnStatus::Present,
                    (false, Presence::Added { .. }) => ColumnStatus::AbsentOptional,
                    (false, Presence::Base) => ColumnStatus::AbsentRequired,
                };
                columns.push(ColumnCoverage {
                    table: spec.name.to_owned(),
                    column: c.name.to_owned(),
                    status,
                    disposition: Some(c.disposition),
                });
            }
            for p in present {
                if spec.column(&p.name).is_none() {
                    columns.push(ColumnCoverage {
                        table: spec.name.to_owned(),
                        column: p.name.clone(),
                        status: ColumnStatus::Unmapped,
                        disposition: None,
                    });
                }
            }
        }
        for (name, cols) in &self.tables {
            if catalog::table(name).is_some() {
                continue;
            }
            if catalog::is_sqlite_internal(name) {
                tables.push(TableCoverage {
                    table: name.clone(),
                    status: TableStatus::SqliteInternal,
                    disposition: Some(Disposition::Dropped {
                        reason: catalog::SQLITE_INTERNAL_REASON,
                    }),
                });
                continue;
            }
            tables.push(TableCoverage {
                table: name.clone(),
                status: TableStatus::Unmapped,
                disposition: None,
            });
            for p in cols {
                columns.push(ColumnCoverage {
                    table: name.clone(),
                    column: p.name.clone(),
                    status: ColumnStatus::Unmapped,
                    disposition: None,
                });
            }
        }
        Coverage {
            tables,
            columns,
            other_objects: self.other_objects.clone(),
        }
    }
}

/// How the file's schema compares with the catalog.
#[derive(Debug, Clone, Serialize)]
pub struct Coverage {
    pub tables: Vec<TableCoverage>,
    pub columns: Vec<ColumnCoverage>,
    /// Views and triggers: the desktop creates none, so any is unexpected.
    pub other_objects: Vec<SchemaObject>,
}

impl Coverage {
    /// Columns or tables of the file that the catalog does not know.
    pub fn unmapped_columns(&self) -> impl Iterator<Item = &ColumnCoverage> {
        self.columns
            .iter()
            .filter(|c| c.status == ColumnStatus::Unmapped)
    }

    pub fn unmapped_tables(&self) -> impl Iterator<Item = &TableCoverage> {
        self.tables
            .iter()
            .filter(|t| t.status == TableStatus::Unmapped)
    }

    /// Base columns (or the required table) missing from the file.
    pub fn missing_required(&self) -> Vec<String> {
        let tables = self
            .tables
            .iter()
            .filter(|t| t.status == TableStatus::AbsentRequired)
            .map(|t| t.table.clone());
        let columns = self
            .columns
            .iter()
            .filter(|c| c.status == ColumnStatus::AbsentRequired)
            .map(|c| format!("{}.{}", c.table, c.column));
        tables.chain(columns).collect()
    }

    /// Number of columns per status.
    pub fn count(&self, status: ColumnStatus) -> usize {
        self.columns.iter().filter(|c| c.status == status).count()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TableCoverage {
    pub table: String,
    pub status: TableStatus,
    /// `None` for an unmapped table.
    pub disposition: Option<Disposition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TableStatus {
    /// A catalog table present in the file.
    Known,
    /// A catalog table missing from the file: it reads as empty.
    AbsentOptional,
    /// `posts` is missing: not a desktop library.
    AbsentRequired,
    /// `sqlite_*`: dropped.
    SqliteInternal,
    /// A table the catalog does not know.
    Unmapped,
}

#[derive(Debug, Clone, Serialize)]
pub struct ColumnCoverage {
    pub table: String,
    pub column: String,
    pub status: ColumnStatus,
    /// `None` for an unmapped column.
    pub disposition: Option<Disposition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnStatus {
    /// In the file and in the catalog.
    Present,
    /// Added by a later desktop migration; this older file lacks it and it
    /// reads as the migration's default.
    AbsentOptional,
    /// A base column missing from the file.
    AbsentRequired,
    /// In the file but not in the catalog.
    Unmapped,
}
