// Interface icons: Lucide (ISC), a single stroke and a single base size for
// the whole chrome. They are imported from here, never from lucide-react
// directly: changing set or stroke weight stays a single change.
import {
  ArrowUpDown,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  CircleDot,
  CopyPlus,
  Database,
  Download,
  Ellipsis,
  Grid3x3,
  Eye,
  Funnel,
  GripVertical,
  KeyRound,
  Layers,
  Link2,
  Lock,
  LockOpen,
  LucideProps,
  Pin,
  Play,
  Plus,
  RefreshCw,
  Search,
  Settings,
  ShieldCheck,
  Square,
  Star,
  RotateCcw,
  Terminal,
  Trash2,
  Table2,
  Upload,
  X,
} from "lucide-react";

const base = { size: 14, strokeWidth: 1.75, "aria-hidden": true } as const;
const big = { size: 20, strokeWidth: 1.6, "aria-hidden": true } as const;

export const IconX = (p: LucideProps) => <X {...base} {...p} />;
export const IconRefresh = (p: LucideProps) => <RefreshCw {...base} {...p} />;
export const IconChevronDown = (p: LucideProps) => <ChevronDown {...base} {...p} />;
export const IconChevronUp = (p: LucideProps) => <ChevronUp {...base} {...p} />;
export const IconChevronRight = (p: LucideProps) => <ChevronRight {...base} {...p} />;
export const IconChevronLeft = (p: LucideProps) => <ChevronLeft {...base} {...p} />;
export const IconPin = (p: LucideProps) => <Pin {...base} size={12} {...p} />;
export const IconStar = (p: LucideProps) => <Star {...base} size={12} {...p} />;
export const IconSearch = (p: LucideProps) => <Search {...base} {...p} />;
export const IconTrash = (p: LucideProps) => <Trash2 {...base} {...p} />;
export const IconPlus = (p: LucideProps) => <Plus {...base} {...p} />;
export const IconMore = (p: LucideProps) => <Ellipsis {...base} {...p} />;
export const IconExport = (p: LucideProps) => <Upload {...base} {...p} />;
export const IconImport = (p: LucideProps) => <Download {...base} {...p} />;
export const IconDuplicate =(p: LucideProps) => <CopyPlus {...base} {...p} />;
export const IconFilter = (p: LucideProps) => <Funnel {...base} size={13} {...p} />;
export const IconCheck = (p: LucideProps) => <Check {...base} size={12} {...p} />;
export const IconLock = (p: LucideProps) => <Lock {...base} {...p} />;
export const IconLockOpen = (p: LucideProps) => <LockOpen {...base} {...p} />;
export const IconShieldCheck = (p: LucideProps) => <ShieldCheck {...base} {...p} />;
export const IconGrip = (p: LucideProps) => <GripVertical {...base} size={13} {...p} />;
export const IconSort = (p: LucideProps) => <ArrowUpDown {...base} size={13} {...p} />;
export const IconKey = (p: LucideProps) => <KeyRound {...base} size={12} {...p} />;
export const IconPlay = (p: LucideProps) => <Play {...base} size={13} {...p} />;
export const IconStop = (p: LucideProps) => <Square {...base} size={12} {...p} />;
/// Tabs and entries of kind "SQL query".
export const IconSql = (p: LucideProps) => <Terminal {...base} {...p} />;
/// A database (a schema on PostgreSQL) by name: the marker in the tab bar.
export const IconDb = (p: LucideProps) => <Database {...base} size={12} {...p} />;
/// Relations: table, view, materialized view, foreign table.
export const IconTable = (p: LucideProps) => <Table2 {...base} {...p} />;
export const IconView = (p: LucideProps) => <Eye {...base} {...p} />;
export const IconMatView = (p: LucideProps) => <Layers {...base} {...p} />;
export const IconForeign = (p: LucideProps) => <Link2 {...base} {...p} />;
/// Activity bar — the same shapes as the mock: grid (schema), circle with
/// a dot (connections), circular arrow (history).
export const IconDatabase = (p: LucideProps) => <Grid3x3 {...big} {...p} />;
export const IconPlug = (p: LucideProps) => <CircleDot {...big} {...p} />;
export const IconHistory = (p: LucideProps) => <RotateCcw {...big} {...p} />;
export const IconSettings = (p: LucideProps) => <Settings {...big} {...p} />;

/// Icon of a relation from its `kind` (introspection).
export function RelIcon({ kind }: { kind: string }) {
  if (kind === "view") return <IconView />;
  if (kind === "materialized_view") return <IconMatView />;
  if (kind === "foreign_table") return <IconForeign />;
  return <IconTable />;
}
