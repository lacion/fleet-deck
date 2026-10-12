// DL-141: the one icon set of the board (lucide-react). Components import icons from here only.
import type { LucideIcon } from 'lucide-react';

export type { LucideIcon } from 'lucide-react';
export {
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
  Copy,
  Ellipsis,
  FileText,
  Folder,
  GitBranch,
  GitCompare,
  Globe,
  Keyboard,
  LayoutGrid,
  Layers,
  LogOut,
  MessageSquare,
  MessagesSquare,
  PanelLeft,
  Paperclip,
  Pencil,
  Play,
  Plug,
  Plus,
  Power,
  Search,
  Server,
  Square,
  SquareTerminal,
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
