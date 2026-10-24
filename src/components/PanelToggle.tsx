interface Props {
  panel: "left" | "bottom" | "right";
  label: string;
  expanded: boolean;
  controls: string;
  disabled?: boolean;
  onClick: () => void;
}

export function PanelToggle({ panel, label, expanded, controls, disabled, onClick }: Props) {
  const title = `${expanded ? "收起" : "展开"}${label}`;
  return (
    <button className="panel-toggle" aria-label={title} title={title}
      aria-expanded={expanded} aria-controls={controls} disabled={disabled} onClick={onClick}>
      <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
        <rect x="1.5" y="2.5" width="13" height="11" rx="1" stroke="currentColor" />
        {panel === "left" && <><path d="M5.5 3v10" stroke="currentColor" />{expanded && <path d="M2 3h3v10H2z" fill="currentColor" opacity=".4" />}</>}
        {panel === "right" && <><path d="M10.5 3v10" stroke="currentColor" />{expanded && <path d="M11 3h3v10h-3z" fill="currentColor" opacity=".4" />}</>}
        {panel === "bottom" && <><path d="M2 9.5h12" stroke="currentColor" />{expanded && <path d="M2 10h12v3H2z" fill="currentColor" opacity=".4" />}</>}
      </svg>
    </button>
  );
}
