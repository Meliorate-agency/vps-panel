"use client";

import { useEffect, useRef, useState } from "react";

type Props = { slug: string; size?: number };

// Render priority:
//   1. Try /public/icons/<slug>.svg — drop the vendor's official logo there
//      and it gets served instead of the inline stylized mark below.
//   2. If that 404s (file absent), fall back to the inline geometric icon
//      defined in this file.
// No manual registry to keep in sync — adding or removing a file in
// /public/icons/ flips the icon automatically.

export default function ServiceIcon({ slug, size = 36 }: Props) {
  const [staticFailed, setStaticFailed] = useState(false);
  const imgRef = useRef<HTMLImageElement>(null);

  // A 404 that lands before hydration never reaches onError (React attaches it
  // too late), which left a broken-image box: check the image once mounted.
  useEffect(() => {
    const img = imgRef.current;
    if (img?.complete && img.naturalWidth === 0) setStaticFailed(true);
  }, []);

  if (!staticFailed) {
    return (
      // eslint-disable-next-line @next/next/no-img-element
      <img
        ref={imgRef}
        src={`/icons/${slug}.svg`}
        alt=""
        width={size}
        height={size}
        style={{ display: "block", objectFit: "contain" }}
        onError={() => setStaticFailed(true)}
      />
    );
  }

  switch (slug) {
    case "mariadb":
      return <Cylinder size={size} color="#7fc4a8" />;
    case "postgresql":
      return <PostgresMark size={size} />;
    case "redis":
      return <Stacked size={size} color="#ff6a55" />;
    case "qdrant":
      return <VectorField size={size} />;
    case "docker":
      return <DockerMark size={size} />;
    case "nginx":
      return <NginxMark size={size} />;
    case "pgadmin":
      return <PgAdminMark size={size} />;
    case "phpmyadmin":
      return <PhpMyAdminMark size={size} />;
    case "roundcube":
      return <RoundcubeMark size={size} />;
    case "n8n":
      return <N8nMark size={size} />;
    case "pgvector":
      return <PgVectorMark size={size} />;
    case "tailscale":
      return <TailscaleMark size={size} />;
    default:
      return <Cylinder size={size} color="var(--text-dim)" />;
  }
}

function TailscaleMark({ size }: { size: number }) {
  // Tailscale's nine dots: the T (top row + middle column) solid, the rest faint.
  const solid = new Set(["0,0", "1,0", "2,0", "1,1", "1,2"]);
  const dots = [0, 1, 2].flatMap((y) => [0, 1, 2].map((x) => [x, y] as const));
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
      {dots.map(([x, y]) => (
        <circle
          key={`${x},${y}`}
          cx={7 + x * 9}
          cy={7 + y * 9}
          r="3.4"
          fill="var(--text)"
          opacity={solid.has(`${x},${y}`) ? 1 : 0.25}
        />
      ))}
    </svg>
  );
}

function PgVectorMark({ size }: { size: number }) {
  // Postgres blue (matches PostgresMark) + an embedding-vector arrow with
  // scattered points, so it reads as "Postgres vectors" and stays visually
  // distinct from Qdrant's magenta VectorField.
  const color = "#6aa1ff";
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
      <line
        x1="6"
        y1="26"
        x2="22"
        y2="8"
        stroke={color}
        strokeWidth="2"
        strokeLinecap="round"
      />
      <path
        d="M22 8 l-5 1 M22 8 l-1 5"
        stroke={color}
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <circle cx="6" cy="26" r="2.4" fill={color} />
      <circle cx="12" cy="12" r="1.8" fill={color} opacity="0.7" />
      <circle cx="25" cy="20" r="1.8" fill={color} opacity="0.7" />
      <circle cx="26" cy="27" r="1.8" fill={color} opacity="0.5" />
    </svg>
  );
}

function N8nMark({ size }: { size: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      xmlns="http://www.w3.org/2000/svg"
      style={{ flex: "none", lineHeight: 1 }}
      aria-label="n8n"
      role="img"
    >
      <title>n8n</title>
      <path
        clipRule="evenodd"
        fillRule="evenodd"
        fill="#EA4B71"
        d="M24 8.4c0 1.325-1.102 2.4-2.462 2.4-1.146 0-2.11-.765-2.384-1.8h-3.436c-.602 0-1.115.424-1.214 1.003l-.101.592a2.38 2.38 0 01-.8 1.405c.412.354.704.844.8 1.405l.1.592A1.222 1.222 0 0015.719 15h.975c.273-1.035 1.237-1.8 2.384-1.8 1.36 0 2.461 1.075 2.461 2.4S20.436 18 19.078 18c-1.147 0-2.11-.765-2.384-1.8h-.975c-1.204 0-2.23-.848-2.428-2.005l-.101-.592a1.222 1.222 0 00-1.214-1.003H10.97c-.308.984-1.246 1.7-2.356 1.7-1.11 0-2.048-.716-2.355-1.7H4.817c-.308.984-1.246 1.7-2.355 1.7C1.102 14.3 0 13.225 0 11.9s1.102-2.4 2.462-2.4c1.183 0 2.172.815 2.408 1.9h1.337c.236-1.085 1.225-1.9 2.408-1.9 1.184 0 2.172.815 2.408 1.9h.952c.601 0 1.115-.424 1.213-1.003l.102-.592c.198-1.157 1.225-2.005 2.428-2.005h3.436c.274-1.035 1.238-1.8 2.384-1.8C22.898 6 24 7.075 24 8.4zm-1.23 0c0 .663-.552 1.2-1.232 1.2-.68 0-1.23-.537-1.23-1.2 0-.663.55-1.2 1.23-1.2.68 0 1.231.537 1.231 1.2zM2.461 13.1c.68 0 1.23-.537 1.23-1.2 0-.663-.55-1.2-1.23-1.2-.68 0-1.231.537-1.231 1.2 0 .663.55 1.2 1.23 1.2zm6.153 0c.68 0 1.231-.537 1.231-1.2 0-.663-.55-1.2-1.23-1.2-.68 0-1.231.537-1.231 1.2 0 .663.55 1.2 1.23 1.2zm10.462 3.7c.68 0 1.23-.537 1.23-1.2 0-.663-.55-1.2-1.23-1.2-.68 0-1.23.537-1.23 1.2 0 .663.55 1.2 1.23 1.2z"
      />
    </svg>
  );
}

function Cylinder({ size, color }: { size: number; color: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
      <ellipse cx="16" cy="7" rx="10" ry="3" stroke={color} strokeWidth="2" />
      <path
        d="M6 7v18c0 1.66 4.48 3 10 3s10-1.34 10-3V7"
        stroke={color}
        strokeWidth="2"
      />
      <ellipse cx="16" cy="16" rx="10" ry="3" stroke={color} strokeWidth="1.5" opacity="0.5" />
      <ellipse cx="16" cy="22" rx="10" ry="3" stroke={color} strokeWidth="1.5" opacity="0.3" />
    </svg>
  );
}

function PostgresMark({ size }: { size: number }) {
  const color = "#6aa1ff";
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
      <circle cx="16" cy="16" r="12" stroke={color} strokeWidth="2" />
      <path
        d="M12 9.5v13M12 9.5h6a3.5 3.5 0 010 7h-6"
        stroke={color}
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function Stacked({ size, color }: { size: number; color: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
      <path
        d="M4 9l12-5 12 5-12 5L4 9z"
        stroke={color}
        strokeWidth="2"
        strokeLinejoin="round"
      />
      <path
        d="M4 16l12 5 12-5"
        stroke={color}
        strokeWidth="2"
        strokeLinejoin="round"
        opacity="0.7"
      />
      <path
        d="M4 23l12 5 12-5"
        stroke={color}
        strokeWidth="2"
        strokeLinejoin="round"
        opacity="0.4"
      />
    </svg>
  );
}

function VectorField({ size }: { size: number }) {
  const color = "var(--accent-glow)";
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
      <line x1="8" y1="8" x2="16" y2="16" stroke={color} strokeWidth="1" opacity="0.5" />
      <line x1="24" y1="8" x2="16" y2="16" stroke={color} strokeWidth="1" opacity="0.5" />
      <line x1="8" y1="24" x2="16" y2="16" stroke={color} strokeWidth="1" opacity="0.5" />
      <line x1="24" y1="24" x2="16" y2="16" stroke={color} strokeWidth="1" opacity="0.5" />
      <circle cx="8" cy="8" r="2.5" fill={color} />
      <circle cx="24" cy="8" r="2.5" fill={color} />
      <circle cx="16" cy="16" r="3" fill={color} />
      <circle cx="8" cy="24" r="2.5" fill={color} />
      <circle cx="24" cy="24" r="2.5" fill={color} />
    </svg>
  );
}

function DockerMark({ size }: { size: number }) {
  const color = "#2496ed";
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
      <rect x="6" y="14" width="3.5" height="3.5" fill={color} />
      <rect x="10.5" y="14" width="3.5" height="3.5" fill={color} />
      <rect x="15" y="14" width="3.5" height="3.5" fill={color} />
      <rect x="19.5" y="14" width="3.5" height="3.5" fill={color} />
      <rect x="10.5" y="9.5" width="3.5" height="3.5" fill={color} />
      <rect x="15" y="9.5" width="3.5" height="3.5" fill={color} />
      <rect x="19.5" y="9.5" width="3.5" height="3.5" fill={color} />
      <rect x="15" y="5" width="3.5" height="3.5" fill={color} />
      <path
        d="M25 19c1.5 0 2.5 1 2.5 2.5 0 2-1.5 4-5 4H8c-2.5 0-4-2-4-4.5"
        stroke={color}
        strokeWidth="2"
        strokeLinecap="round"
        fill="none"
      />
    </svg>
  );
}

function NginxMark({ size }: { size: number }) {
  const color = "#009639";
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
      <polygon
        points="16,3 27,9.5 27,22.5 16,29 5,22.5 5,9.5"
        stroke={color}
        strokeWidth="2"
        strokeLinejoin="round"
      />
      <path
        d="M11 22 L11 10 L21 22 L21 10"
        stroke={color}
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function PgAdminMark({ size }: { size: number }) {
  const color = "#336791";
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
      <rect x="4" y="6" width="24" height="20" rx="2" stroke={color} strokeWidth="2" />
      <line x1="4" y1="11" x2="28" y2="11" stroke={color} strokeWidth="1.5" />
      <circle cx="7" cy="8.5" r="0.8" fill={color} />
      <circle cx="9.5" cy="8.5" r="0.8" fill={color} />
      <circle cx="12" cy="8.5" r="0.8" fill={color} />
      <path
        d="M12 16v6M12 16h4a2.5 2.5 0 010 5h-4"
        stroke={color}
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function RoundcubeMark({ size }: { size: number }) {
  const color = "#37beff";
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
      <rect
        x="4"
        y="8"
        width="24"
        height="17"
        rx="2"
        stroke={color}
        strokeWidth="2"
        strokeLinejoin="round"
      />
      <path
        d="M4 10 L16 19 L28 10"
        stroke={color}
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <circle cx="16" cy="16.5" r="2.5" fill={color} opacity="0.25" />
    </svg>
  );
}

function PhpMyAdminMark({ size }: { size: number }) {
  const color = "#f29111";
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none" aria-hidden="true">
      <rect x="4" y="6" width="24" height="20" rx="2" stroke={color} strokeWidth="2" />
      <line x1="4" y1="11" x2="28" y2="11" stroke={color} strokeWidth="1.5" />
      <circle cx="7" cy="8.5" r="0.8" fill={color} />
      <circle cx="9.5" cy="8.5" r="0.8" fill={color} />
      <circle cx="12" cy="8.5" r="0.8" fill={color} />
      <ellipse cx="16" cy="15" rx="6" ry="1.5" stroke={color} strokeWidth="1.5" />
      <path
        d="M10 15v6c0 0.83 2.69 1.5 6 1.5s6-0.67 6-1.5v-6"
        stroke={color}
        strokeWidth="1.5"
      />
      <ellipse
        cx="16"
        cy="18"
        rx="6"
        ry="1.5"
        stroke={color}
        strokeWidth="1"
        opacity="0.6"
      />
    </svg>
  );
}
