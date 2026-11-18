//! Lucide icons (ISC, see assets/icons/LICENSE-lucide), drawn in the current text color.

use gpui::{Hsla, Svg, prelude::*, svg};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    ArrowDown,
    ArrowLeft,
    ArrowRightLeft,
    ArrowUp,
    Brain,
    Check,
    ChevronDown,
    ChevronRight,
    ChevronsUpDown,
    Circle,
    CircleAlert,
    CircleDot,
    Claude,
    Copy,
    Ellipsis,
    Eye,
    FileText,
    Folder,
    FolderGit,
    FolderOpen,
    GitBranch,
    GitPullRequest,
    Globe,
    GripVertical,
    Info,
    KeyRound,
    List,
    ListTodo,
    Loader,
    Lock,
    MessageSquare,
    PanelLeft,
    PenLine,
    Pencil,
    Plus,
    RefreshCw,
    Search,
    Settings,
    Square,
    SquareKanban,
    SquarePen,
    SquareTerminal,
    Trash,
    Workflow,
    Wrench,
    X,
}

impl Icon {
    pub fn path(self) -> &'static str {
        match self {
            Icon::ArrowDown => "icons/arrow-down.svg",
            Icon::ArrowLeft => "icons/arrow-left.svg",
            Icon::ArrowRightLeft => "icons/arrow-right-left.svg",
            Icon::ArrowUp => "icons/arrow-up.svg",
            Icon::Brain => "icons/brain.svg",
            Icon::Check => "icons/check.svg",
            Icon::ChevronDown => "icons/chevron-down.svg",
            Icon::ChevronRight => "icons/chevron-right.svg",
            Icon::ChevronsUpDown => "icons/chevrons-up-down.svg",
            Icon::Circle => "icons/circle.svg",
            Icon::CircleAlert => "icons/circle-alert.svg",
            Icon::CircleDot => "icons/circle-dot.svg",
            Icon::Claude => "icons/claude.svg",
            Icon::Copy => "icons/copy.svg",
            Icon::Ellipsis => "icons/ellipsis.svg",
            Icon::Eye => "icons/eye.svg",
            Icon::FileText => "icons/file-text.svg",
            Icon::Folder => "icons/folder.svg",
            Icon::FolderGit => "icons/folder-git-2.svg",
            Icon::FolderOpen => "icons/folder-open.svg",
            Icon::GitBranch => "icons/git-branch.svg",
            Icon::GitPullRequest => "icons/git-pull-request.svg",
            Icon::Globe => "icons/globe.svg",
            Icon::GripVertical => "icons/grip-vertical.svg",
            Icon::Info => "icons/info.svg",
            Icon::KeyRound => "icons/key-round.svg",
            Icon::List => "icons/list.svg",
            Icon::ListTodo => "icons/list-todo.svg",
            Icon::Loader => "icons/loader-circle.svg",
            Icon::Lock => "icons/lock.svg",
            Icon::MessageSquare => "icons/message-square.svg",
            Icon::PanelLeft => "icons/panel-left.svg",
            Icon::PenLine => "icons/pen-line.svg",
            Icon::Pencil => "icons/pencil.svg",
            Icon::Plus => "icons/plus.svg",
            Icon::RefreshCw => "icons/refresh-cw.svg",
            Icon::Search => "icons/search.svg",
            Icon::Settings => "icons/settings.svg",
            Icon::Square => "icons/square.svg",
            Icon::SquareKanban => "icons/square-kanban.svg",
            Icon::SquarePen => "icons/square-pen.svg",
            Icon::SquareTerminal => "icons/square-terminal.svg",
            Icon::Trash => "icons/trash.svg",
            Icon::Workflow => "icons/workflow.svg",
            Icon::Wrench => "icons/wrench.svg",
            Icon::X => "icons/x.svg",
        }
    }

    /// The icon for an ACP tool kind.
    pub fn for_tool(kind: &str) -> Icon {
        match kind {
            "read" => Icon::FileText,
            "edit" => Icon::Pencil,
            "delete" => Icon::Trash,
            "move" => Icon::ArrowRightLeft,
            "search" => Icon::Search,
            "execute" => Icon::SquareTerminal,
            "think" => Icon::Brain,
            "fetch" => Icon::Globe,
            _ => Icon::Wrench,
        }
    }
}

/// A 16px icon in `color`; gpui's svg does not inherit the text color.
pub fn icon(icon: Icon, color: Hsla) -> Svg {
    svg().path(icon.path()).size_4().flex_none().text_color(color)
}
