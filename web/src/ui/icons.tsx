// DL-141: the one icon set of the board (lucide-react). Components import icons from here only.
import type { LucideIcon } from 'lucide-react';

export type { LucideIcon } from 'lucide-react';
export {
  Archive,
  ArchiveRestore,
  ArrowDown,
  ArrowUp,
  Ban,
  Bell,
  BellOff,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  CircleHelp,
  Code,
  Copy,
  Ellipsis,
  Eye,
  FileText,
  Folder,
  GitBranch,
  GitCompare,
  Globe,
  Keyboard,
  Layers,
  LayoutGrid,
  Link,
  LogOut,
  Mail,
  MessageSquare,
  MessagesSquare,
  Monitor,
  Moon,
  PanelLeft,
  Paperclip,
  Pencil,
  Pin,
  PinOff,
  Play,
  Plug,
  Plus,
  Power,
  RotateCcw,
  Rows3,
  Search,
  Send,
  Server,
  Square,
  SquareTerminal,
  Star,
  Sun,
  TriangleAlert,
  Undo2,
  Workflow,
  Wrench,
  X,
  Zap,
} from 'lucide-react';

/** One icon: 16 px, stroke 1.75, hidden from assistive technology (the control carries the name). */
export function Icon({ of: Of, size = 16 }: { readonly of: LucideIcon; readonly size?: number }) {
  return (
    <Of size={size} strokeWidth={1.75} aria-hidden="true" focusable="false" className="fd-icon" />
  );
}
