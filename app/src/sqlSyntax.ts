// SQL syntax of an engine for the UI's editors (`EngineOut.sql_syntax`):
// CodeMirror dialect (highlighting, keywords), identifier quoting and the
// schema unqualified names resolve in. A syntax not listed here gets
// standard SQL, with every name that is not plain lowercase quoted.
import { MySQL, PostgreSQL, SQLDialect, StandardSQL } from "@codemirror/lang-sql";
import { engineOf } from "./engines";

export type SqlSyntax = {
  lang: SQLDialect;
  quote: string;
  /// Names the server reads as written without quotes.
  bare: RegExp;
  /// Schema of unqualified names; null = the current database.
  defaultSchema: string | null;
};

const SYNTAXES: Record<string, SqlSyntax> = {
  // PG folds bare names to lowercase; MySQL keeps them as written.
  postgres: { lang: PostgreSQL, quote: '"', bare: /^[a-z_][a-z0-9_$]*$/, defaultSchema: "public" },
  mysql: { lang: MySQL, quote: "`", bare: /^[A-Za-z_][A-Za-z0-9_$]*$/, defaultSchema: null },
};

const STANDARD: SqlSyntax = { lang: StandardSQL, quote: '"', bare: /^[a-z_][a-z0-9_$]*$/, defaultSchema: null };

/// The syntax of a profile's engine.
export const sqlSyntax = (dialect: string): SqlSyntax => SYNTAXES[engineOf(dialect)?.sql_syntax ?? ""] ?? STANDARD;

/// Name to write in the SQL: bare if the server reads it the same, otherwise
/// between the syntax's quotes.
export function quoteIfNeeded(name: string, s: SqlSyntax): string {
  return s.bare.test(name) ? name : s.quote + name.replaceAll(s.quote, s.quote + s.quote) + s.quote;
}
