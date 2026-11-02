// Database engines of this build (architecture §2.2): the backend's driver
// registry describes them, the UI does not list engines by name.
import { EngineOut, ipc } from "./ipc";

let list: EngineOut[] = [];

/// Before the first render (App): the dialogs read the list synchronously.
export async function loadEngines(): Promise<void> {
  list = await ipc.engines();
}

export const engines = (): readonly EngineOut[] => list;

/// The profile's engine; undefined for one this build does not have.
export const engineOf = (dialect: string): EngineOut | undefined => list.find((e) => e.id === dialect);

/// Name in the dialogs ("MySQL / MariaDB"); the raw id for an unknown engine.
export const engineName = (dialect: string): string => engineOf(dialect)?.name ?? dialect;

/// Name next to a host in the lists ("MySQL").
export const engineShortName = (dialect: string): string => engineOf(dialect)?.short_name ?? dialect;

/// The database belongs to the tab and is switched with `USE` before running
/// (MySQL); otherwise it is fixed at connect (PG, architecture §3.3).
export const canSwitchDb = (dialect: string | undefined): boolean =>
  dialect !== undefined && engineOf(dialect)?.can_change_database_in_session === true;

/// False where a DDL commits what was pending and Rollback cannot undo it
/// (MySQL, §9.7.1).
export const transactionalDdl = (dialect: string): boolean => engineOf(dialect)?.transactional_ddl !== false;
