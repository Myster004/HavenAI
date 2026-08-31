import { useCallback, useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import {
  AlertCircle,
  Check,
  Download,
  Eye,
  ExternalLink,
  Heart,
  Loader2,
  MessageCircle,
  Shield,
} from "lucide-react";
import { cn } from "../../design-tokens";
import { useI18n } from "../../../core/i18n/context";
import {
  fetchDiscoverCharacterDetail,
  formatCount,
  getDiscoverImportStatus,
  importDiscoverCharacter,
} from "../../../core/discovery/chub/chubApi";
import type { ChubCharacterDetail } from "../../../core/discovery/chub/chubTypes";

type ImportState =
  | { phase: "idle" }
  | { phase: "importing" }
  | { phase: "done"; characterId: string }
  | { phase: "error"; message: string };

function StatCard({
  icon: Icon,
  label,
  value,
}: {
  icon: typeof Heart;
  label: string;
  value: string;
}) {
  return (
    <div className="flex flex-col gap-0.5 rounded-xl border border-fg/10 bg-fg/[0.03] px-3 py-2">
      <span className="flex items-center gap-1 text-[10px] uppercase tracking-wider text-fg/40">
        <Icon className="h-3 w-3" />
        {label}
      </span>
      <span className="text-sm font-semibold text-fg">{value}</span>
    </div>
  );
}

function DetailSection({ title, content }: { title: string; content: string }) {
  if (!content.trim()) return null;
  return (
    <section>
      <h3 className="mb-1.5 text-sm font-semibold text-fg">{title}</h3>
      <p className="whitespace-pre-wrap text-sm leading-relaxed text-fg/70">{content}</p>
    </section>
  );
}

export function ChubCharacterDetailPage() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const { t } = useI18n();

  const fullPath = id ? decodeURIComponent(id) : "";
  const [detail, setDetail] = useState<ChubCharacterDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [importState, setImportState] = useState<ImportState>({ phase: "idle" });
  const [showNsfwImage, setShowNsfwImage] = useState(false);

  useEffect(() => {
    if (!fullPath) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    setDetail(null);
    setImportState({ phase: "idle" });

    Promise.all([
      fetchDiscoverCharacterDetail(fullPath),
      getDiscoverImportStatus(fullPath).catch(() => null),
    ])
      .then(([detailData, importedId]) => {
        if (cancelled) return;
        setDetail(detailData);
        setShowNsfwImage(!detailData.isNsfw);
        if (importedId) setImportState({ phase: "done", characterId: importedId });
      })
      .catch((err) => {
        if (cancelled) return;
        console.error("Failed to load character detail:", err);
        setError(
          typeof err === "string"
            ? err
            : "Unable to load this character right now. Please try again.",
        );
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [fullPath]);

  const handleDownload = useCallback(async () => {
    if (!fullPath || importState.phase === "importing" || importState.phase === "done") return;
    setImportState({ phase: "importing" });
    try {
      const characterId = await importDiscoverCharacter(fullPath);
      setImportState({ phase: "done", characterId });
    } catch (err) {
      console.error("Character download failed:", err);
      setImportState({
        phase: "error",
        message:
          typeof err === "string"
            ? err
            : "Character download failed. Please try again.",
      });
    }
  }, [fullPath, importState.phase]);

  const hue =
    (detail?.name ?? "?")
      .split("")
      .reduce((acc, char) => acc + char.charCodeAt(0), 0) % 360;
  const fallbackGradient = `linear-gradient(160deg, hsl(${hue}, 45%, 18%) 0%, hsl(${(hue + 50) % 360}, 35%, 12%) 100%)`;
  const blurImage = (detail?.isNsfw ?? false) && !showNsfwImage;

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center bg-surface">
        <Loader2 className="h-8 w-8 animate-spin text-fg/40" />
      </div>
    );
  }

  if (error || !detail) {
    return (
      <div className="flex h-full flex-col items-center justify-center bg-surface px-6 py-20">
        <div className="mb-4 flex h-16 w-16 items-center justify-center rounded-2xl border border-danger/30 bg-danger/10">
          <AlertCircle className="h-8 w-8 text-danger" />
        </div>
        <h3 className="mb-2 text-lg font-semibold text-fg">{t("discovery.errorTitle")}</h3>
        <p className="mb-6 text-center text-sm text-fg/50">{error ?? "Character unavailable."}</p>
        <button
          type="button"
          onClick={() => navigate("/discover")}
          className="rounded-xl border border-fg/20 bg-fg/10 px-5 py-2.5 text-sm font-medium text-fg transition-all hover:bg-fg/15 active:scale-95"
        >
          Back to Discover
        </button>
      </div>
    );
  }

  const imported = importState.phase === "done";

  return (
    <div
      className="mx-auto w-full max-w-5xl px-4 pb-24 pt-6 lg:px-8"
      style={{
        paddingBottom: "calc(env(safe-area-inset-bottom) + 96px)",
      }}
    >
      <div className="grid gap-6 lg:grid-cols-[300px_1fr]">
        {/* Left: image */}
        <div>
          <div
            className="relative aspect-3/4 w-full overflow-hidden rounded-2xl border border-fg/10"
            style={{ background: fallbackGradient }}
          >
            {detail.avatarUrl && (
              <img
                src={detail.avatarUrl}
                alt={detail.name}
                loading="lazy"
                className={cn(
                  "absolute inset-0 h-full w-full object-cover transition-opacity",
                  blurImage && "blur-xl",
                )}
                onError={(event) => {
                  (event.target as HTMLImageElement).style.display = "none";
                }}
              />
            )}
            {blurImage && (
              <button
                type="button"
                onClick={() => setShowNsfwImage(true)}
                className="absolute inset-0 z-10 flex flex-col items-center justify-center gap-2 bg-black/50"
              >
                <Shield className="h-8 w-8 text-danger" />
                <span className="text-xs font-bold uppercase tracking-wider text-danger">
                  NSFW — tap to view
                </span>
              </button>
            )}
          </div>

          {/* Download */}
          <div className="mt-4 space-y-2">
            {imported ? (
              <>
                <div className="flex h-11 w-full items-center justify-center gap-2 rounded-xl border border-accent/40 bg-accent/15 text-sm font-semibold text-accent">
                  <Check className="h-4 w-4" />
                  In Library
                </div>
                <button
                  type="button"
                  onClick={() => navigate(`/chat/${importState.phase === "done" ? importState.characterId : ""}`)}
                  className="h-11 w-full rounded-xl bg-fg text-sm font-semibold text-surface shadow-lg shadow-fg/20 transition-all hover:opacity-90 active:scale-[0.98]"
                >
                  Start Chat
                </button>
                <button
                  type="button"
                  onClick={() => navigate("/library")}
                  className="h-10 w-full rounded-xl border border-fg/10 bg-fg/5 text-sm font-medium text-fg/70 transition-all hover:bg-fg/10 hover:text-fg"
                >
                  Open Library
                </button>
              </>
            ) : (
              <>
                <button
                  type="button"
                  onClick={handleDownload}
                  disabled={importState.phase === "importing"}
                  className="flex h-11 w-full items-center justify-center gap-2 rounded-xl bg-fg text-sm font-semibold text-surface shadow-lg shadow-fg/20 transition-all hover:opacity-90 active:scale-[0.98] disabled:cursor-not-allowed disabled:opacity-60"
                >
                  {importState.phase === "importing" ? (
                    <>
                      <Loader2 className="h-4 w-4 animate-spin" />
                      Downloading...
                    </>
                  ) : (
                    <>
                      <Download className="h-4 w-4" />
                      Download Character
                    </>
                  )}
                </button>
                {importState.phase === "error" && (
                  <p className="text-center text-xs text-danger">{importState.message}</p>
                )}
              </>
            )}
          </div>
        </div>

        {/* Right: info */}
        <div className="min-w-0 space-y-5">
          <div>
            <div className="flex flex-wrap items-center gap-2">
              {detail.isNsfw && (
                <span className="flex items-center gap-1 rounded-full bg-danger/90 px-2 py-0.5 text-[9px] font-bold uppercase tracking-wider text-fg">
                  <Shield className="h-2.5 w-2.5" />
                  NSFW
                </span>
              )}
              <span className="rounded-full border border-fg/15 bg-fg/5 px-2 py-0.5 text-[10px] font-medium text-fg/50">
                Chub AI
              </span>
            </div>
            <h1 className="mt-2 text-2xl font-bold leading-tight tracking-tight text-fg">
              {detail.name}
            </h1>
            {detail.creator && (
              <p className="mt-0.5 text-sm text-fg/50">
                by @{detail.creatorUsername ?? detail.creator}
              </p>
            )}
            {detail.tagline && (
              <p className="mt-2 text-sm leading-relaxed text-fg/70">{detail.tagline}</p>
            )}
          </div>

          {/* Stats */}
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
            {detail.likes !== undefined && detail.likes > 0 && (
              <StatCard icon={Heart} label="Likes" value={formatCount(detail.likes)} />
            )}
            {detail.downloads !== undefined && detail.downloads > 0 && (
              <StatCard icon={Download} label="Downloads" value={formatCount(detail.downloads)} />
            )}
            {detail.views !== undefined && detail.views > 0 && (
              <StatCard icon={Eye} label="Views" value={formatCount(detail.views)} />
            )}
            {detail.messages !== undefined && detail.messages > 0 && (
              <StatCard icon={MessageCircle} label="Messages" value={formatCount(detail.messages)} />
            )}
          </div>

          {/* Tags */}
          {detail.tags.length > 0 && (
            <div className="flex flex-wrap gap-1.5">
              {detail.tags.map((tag) => (
                <span
                  key={tag}
                  className="rounded-full bg-fg/10 px-2.5 py-1 text-[11px] font-medium text-fg/70"
                >
                  {tag}
                </span>
              ))}
            </div>
          )}

          {/* Definition sections — hidden when missing */}
          <div className="space-y-4">
            <DetailSection title="Description" content={detail.description ?? ""} />
            <DetailSection title="Personality" content={detail.personality ?? ""} />
            <DetailSection title="Scenario" content={detail.scenario ?? ""} />
            <DetailSection title="Creator Notes" content={detail.creatorNotes ?? ""} />
            <DetailSection title="First Message" content={detail.firstMessage ?? ""} />
            <DetailSection title="Example Dialogue" content={detail.exampleDialogue ?? ""} />
            <DetailSection title="System Prompt" content={detail.systemPrompt ?? ""} />
            <DetailSection
              title="Post-History Instructions"
              content={detail.postHistoryInstructions ?? ""}
            />

            {detail.alternateGreetings.length > 0 && (
              <details className="rounded-xl border border-fg/10 bg-fg/[0.03] p-3">
                <summary className="cursor-pointer text-sm font-semibold text-fg">
                  Alternate Greetings ({detail.alternateGreetings.length})
                </summary>
                <div className="mt-2 space-y-3">
                  {detail.alternateGreetings.map((greeting, index) => (
                    <p
                      key={index}
                      className="whitespace-pre-wrap text-sm leading-relaxed text-fg/70"
                    >
                      {greeting}
                    </p>
                  ))}
                </div>
              </details>
            )}

            {detail.characterBookEntryCount !== undefined && (
              <div className="rounded-xl border border-fg/10 bg-fg/[0.03] px-3 py-2.5 text-sm text-fg/70">
                <span className="font-semibold text-fg">Character Book</span>
                {detail.characterBookName ? ` — ${detail.characterBookName}` : ""} ·{" "}
                {detail.characterBookEntryCount} entries (imported with the character)
              </div>
            )}
          </div>

          {/* Source attribution */}
          {detail.sourceUrl && (
            <div className="flex items-center gap-2 border-t border-fg/10 pt-4 text-xs text-fg/40">
              <span>Source:</span>
              <a
                href={detail.sourceUrl}
                target="_blank"
                rel="noreferrer noopener"
                className="flex items-center gap-1 text-fg/60 underline-offset-2 hover:text-fg hover:underline"
              >
                Chub AI
                <ExternalLink className="h-3 w-3" />
              </a>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

export default ChubCharacterDetailPage;
