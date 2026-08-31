import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import {
  AlertCircle,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Loader2,
  RefreshCw,
  Search,
  Shield,
  Tag as TagIcon,
  X,
} from "lucide-react";
import { cn } from "../../design-tokens";
import { useI18n } from "../../../core/i18n/context";
import { PageHeader, useInlineHeader } from "../../components/App";
import { useShowNsfwImages } from "./hooks/useDiscoveryNsfw";
import { getAppState } from "../../../core/storage/appState";
import { DiscoverCharacterCard } from "./components/DiscoverCharacterCard";
import {
  DISCOVER_PAGE_SIZE,
  DISCOVER_SORT_OPTIONS,
  type DiscoverCharacter,
  type DiscoverSearchResult,
  type DiscoverSort,
  type DiscoverTag,
} from "../../../core/discovery/chub/chubTypes";
import {
  clearDiscoverTagCache,
  fetchDiscoverTags,
  searchDiscoverCharacters,
} from "../../../core/discovery/chub/chubApi";

const SEARCH_DEBOUNCE_MS = 300;

function parseTagsParam(raw: string | null): string[] {
  if (!raw) return [];
  return raw
    .split(",")
    .map((tag) => tag.trim())
    .filter(Boolean)
    .slice(0, 8);
}

function SkeletonCard() {
  return (
    <div className="flex flex-col overflow-hidden rounded-xl border border-fg/10 bg-fg/[0.02]">
      <div className="aspect-3/4 w-full animate-pulse bg-fg/10" />
      <div className="space-y-2 p-2.5">
        <div className="h-4 w-3/4 rounded bg-fg/15" />
        <div className="h-3 w-1/3 rounded bg-fg/10" />
        <div className="h-3 w-full rounded bg-fg/10" />
      </div>
    </div>
  );
}

export function DiscoveryPage() {
  const navigate = useNavigate();
  const { t } = useI18n();
  const [searchParams, setSearchParams] = useSearchParams();
  const inlineHeader = useInlineHeader();
  const showNsfw = useShowNsfwImages();

  // --- URL-driven state -----------------------------------------------------
  const urlQuery = searchParams.get("q") ?? "";
  const urlTags = useMemo(() => parseTagsParam(searchParams.get("tags")), [searchParams]);
  const urlSort = useMemo(() => {
    const raw = searchParams.get("sort") as DiscoverSort | null;
    return raw && DISCOVER_SORT_OPTIONS.some((o) => o.value === raw) ? raw : "trending";
  }, [searchParams]);
  const urlPage = useMemo(() => {
    const parsed = Number.parseInt(searchParams.get("page") ?? "1", 10);
    return Number.isFinite(parsed) && parsed > 0 ? parsed : 1;
  }, [searchParams]);

  // --- Local UI state -------------------------------------------------------
  const [queryInput, setQueryInput] = useState(urlQuery);
  const [results, setResults] = useState<DiscoverSearchResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [softLoading, setSoftLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [reloadToken, setReloadToken] = useState(0);

  const [tagPanelOpen, setTagPanelOpen] = useState(false);
  const [tagSearch, setTagSearch] = useState("");
  const [availableTags, setAvailableTags] = useState<DiscoverTag[]>([]);
  const [tagsLoading, setTagsLoading] = useState(false);
  const [tagsError, setTagsError] = useState<string | null>(null);
  const [pureModeActive, setPureModeActive] = useState(false);
  const tagPanelRef = useRef<HTMLDivElement | null>(null);

  const requestIdRef = useRef(0);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pendingRefreshRef = useRef(false);
  const tagRefreshRef = useRef(false);

  // Detect Pure Mode so the user understands NSFW cards are hidden server-side
  useEffect(() => {
    let cancelled = false;
    getAppState()
      .then((state) => {
        if (!cancelled) setPureModeActive(Boolean(state.pureModeEnabled));
      })
      .catch(() => {});
    const handler = () => {
      getAppState()
        .then((state) => setPureModeActive(Boolean(state.pureModeEnabled)))
        .catch(() => {});
    };
    window.addEventListener("lettuceai:settings-updated", handler);
    return () => {
      cancelled = true;
      window.removeEventListener("lettuceai:settings-updated", handler);
    };
  }, []);

  const updateParams = useCallback(
    (patch: { q?: string; tags?: string[]; page?: number; sort?: DiscoverSort }) => {
      const next = new URLSearchParams(searchParams);
      if (patch.q !== undefined) {
        if (patch.q.trim()) next.set("q", patch.q.trim());
        else next.delete("q");
      }
      if (patch.tags !== undefined) {
        if (patch.tags.length > 0) next.set("tags", patch.tags.join(","));
        else next.delete("tags");
      }
      if (patch.sort !== undefined) {
        if (patch.sort !== "trending") next.set("sort", patch.sort);
        else next.delete("sort");
      }
      if (patch.page !== undefined) {
        if (patch.page > 1) next.set("page", String(patch.page));
        else next.delete("page");
      }
      setSearchParams(next, { replace: true });
    },
    [searchParams, setSearchParams],
  );

  // Debounce search input → URL
  useEffect(() => {
    if (queryInput === urlQuery) return;
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => {
      updateParams({ q: queryInput, page: 1 });
    }, SEARCH_DEBOUNCE_MS);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [queryInput]);

  const submitSearchNow = useCallback(() => {
    if (debounceRef.current) clearTimeout(debounceRef.current);
    updateParams({ q: queryInput, page: 1 });
  }, [queryInput, updateParams]);

  // --- Fetch results whenever URL state changes ------------------------------
  useEffect(() => {
    const requestId = ++requestIdRef.current;
    const bypassCache = pendingRefreshRef.current;
    pendingRefreshRef.current = false;
    const hasPrevious = results !== null;
    if (hasPrevious) setSoftLoading(true);
    else setLoading(true);
    setError(null);

    searchDiscoverCharacters({
      query: urlQuery || undefined,
      tags: urlTags,
      page: urlPage,
      sort: urlSort,
      refresh: bypassCache,
    })
      .then((data) => {
        if (requestIdRef.current !== requestId) return;
        setResults(data);
      })
      .catch((err) => {
        if (requestIdRef.current !== requestId) return;
        console.error("Discover search failed:", err);
        setError(
          typeof err === "string"
            ? err
            : "Unable to load characters right now. Please try again.",
        );
      })
      .finally(() => {
        if (requestIdRef.current !== requestId) return;
        setLoading(false);
        setSoftLoading(false);
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [urlQuery, urlTags, urlPage, urlSort, reloadToken]);

  // Clamp page if it exceeds total pages after a result set arrives
  useEffect(() => {
    if (results?.totalPages && urlPage > results.totalPages && results.totalPages > 0) {
      updateParams({ page: results.totalPages });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [results?.totalPages, urlPage]);

  // --- Tag panel -------------------------------------------------------------
  const loadTags = useCallback(() => {
    if (availableTags.length > 0 || tagsLoading) return;
    const refresh = tagRefreshRef.current;
    tagRefreshRef.current = false;
    setTagsLoading(true);
    setTagsError(null);
    fetchDiscoverTags("", refresh)
      .then((tags) => setAvailableTags(tags))
      .catch((err) => {
        console.error("Failed to load tags:", err);
        setTagsError(typeof err === "string" ? err : "Unable to load tags right now.");
      })
      .finally(() => setTagsLoading(false));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [availableTags.length, tagsLoading]);

  const openTagPanel = () => {
    if (!tagPanelOpen) loadTags();
    setTagPanelOpen((open) => !open);
  };

  // Close tag panel on outside click
  useEffect(() => {
    if (!tagPanelOpen) return;
    const handler = (event: MouseEvent) => {
      if (tagPanelRef.current && !tagPanelRef.current.contains(event.target as Node)) {
        setTagPanelOpen(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [tagPanelOpen]);

  const toggleTag = useCallback(
    (tag: string) => {
      const next = urlTags.includes(tag)
        ? urlTags.filter((t) => t !== tag)
        : [...urlTags, tag].slice(0, 8);
      updateParams({ tags: next, page: 1 });
    },
    [urlTags, updateParams],
  );

  const filteredAvailableTags = useMemo(() => {
    const needle = tagSearch.trim().toLowerCase();
    const selected = new Set(urlTags);
    return availableTags
      .filter((tag) => !selected.has(tag.name))
      .filter((tag) => !needle || tag.name.toLowerCase().includes(needle))
      .slice(0, 60);
  }, [availableTags, tagSearch, urlTags]);

  const hasActiveFilters = urlQuery.trim().length > 0 || urlTags.length > 0;

  const clearFilters = () => {
    setQueryInput("");
    updateParams({ q: "", tags: [], page: 1 });
  };

  const handleCardClick = useCallback(
    (character: DiscoverCharacter) => {
      if (!character.fullPath) return;
      navigate(`/discover/character/${encodeURIComponent(character.fullPath)}`);
    },
    [navigate],
  );

  const handleRefresh = useCallback(() => {
    pendingRefreshRef.current = true;
    tagRefreshRef.current = true;
    clearDiscoverTagCache();
    setAvailableTags([]);
    setTagSearch("");
    setReloadToken((token) => token + 1);
  }, []);

  const totalPages = results?.totalPages;
  const isLastPage =
    loading ||
    (totalPages !== undefined && urlPage >= totalPages) ||
    (results !== null && results.characters.length < DISCOVER_PAGE_SIZE);

  const goToPage = (page: number) => {
    const clamped =
      totalPages !== undefined && totalPages > 0
        ? Math.max(1, Math.min(page, totalPages))
        : Math.max(1, page);
    if (clamped !== urlPage) updateParams({ page: clamped });
  };

  const cards = results ? results.characters.slice(0, DISCOVER_PAGE_SIZE) : [];

  return (
    <div className="flex h-full flex-col bg-surface lg:px-4">
      <main
        className="flex-1 overflow-y-auto mx-auto w-full lg:max-w-[1600px]"
        style={{
          paddingBottom: "calc(env(safe-area-inset-bottom) + 80px)",
        }}
      >
        {inlineHeader && <PageHeader title={t("common.bottomNav.discover")} />}

        <div className="px-4 pt-4 lg:px-8">
          {/* Search */}
          <div className="relative max-w-xl">
            <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-fg/40" />
            <input
              type="text"
              value={queryInput}
              onChange={(event) => setQueryInput(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") submitSearchNow();
              }}
              placeholder="Search..."
              className={cn(
                "h-10 w-full rounded-xl border border-fg/10 bg-fg/5 pl-9 pr-8 text-sm text-fg outline-none",
                "placeholder:text-fg/40 focus:border-fg/25 focus:bg-fg/[0.07]",
                "transition-all",
              )}
            />
            {queryInput && (
              <button
                type="button"
                onClick={() => {
                  setQueryInput("");
                  updateParams({ q: "", page: 1 });
                }}
                className="absolute right-2 top-1/2 -translate-y-1/2 rounded-full p-1 text-fg/40 hover:bg-fg/10 hover:text-fg"
                aria-label="Clear search"
              >
                <X className="h-3.5 w-3.5" />
              </button>
            )}
          </div>

          {/* Controls: tags selector + sort + chips + clear */}
          <div className="mt-3 flex flex-wrap items-center gap-2">
            {/* Tag selector */}
            <div className="relative" ref={tagPanelRef}>
              <button
                type="button"
                onClick={openTagPanel}
                className={cn(
                  "flex h-9 items-center gap-1.5 rounded-xl border px-3 text-sm font-medium transition-all",
                  urlTags.length > 0
                    ? "border-fg/30 bg-fg/15 text-fg"
                    : "border-fg/10 bg-fg/5 text-fg/70 hover:bg-fg/10 hover:text-fg",
                )}
              >
                <TagIcon className="h-3.5 w-3.5" />
                Tags
                {urlTags.length > 0 && (
                  <span className="rounded-full bg-fg/20 px-1.5 text-[10px] font-semibold">
                    {urlTags.length}
                  </span>
                )}
                <ChevronDown
                  className={cn("h-3.5 w-3.5 transition-transform", tagPanelOpen && "rotate-180")}
                />
              </button>

              {tagPanelOpen && (
                <div className="absolute left-0 top-11 z-30 w-72 rounded-xl border border-fg/15 bg-surface-el shadow-2xl backdrop-blur-md">
                  <div className="border-b border-fg/10 p-2.5">
                    <input
                      type="text"
                      value={tagSearch}
                      onChange={(event) => setTagSearch(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter") {
                          const value = tagSearch.trim();
                          if (value && !urlTags.includes(value)) {
                            toggleTag(value);
                          }
                          setTagSearch("");
                        }
                      }}
                      placeholder="Search tags..."
                      className="h-8 w-full rounded-lg border border-fg/10 bg-fg/5 px-2.5 text-xs text-fg outline-none placeholder:text-fg/40 focus:border-fg/25"
                    />
                    {tagSearch.trim() && !availableTags.some((t) => t.name.toLowerCase() === tagSearch.trim().toLowerCase()) && (
                      <button
                        type="button"
                        onClick={() => {
                          const value = tagSearch.trim();
                          if (value && !urlTags.includes(value)) toggleTag(value);
                          setTagSearch("");
                        }}
                        className="mt-1.5 flex w-full items-center gap-1.5 rounded-lg px-2 py-1 text-left text-xs text-accent transition-colors hover:bg-accent/10"
                      >
                        <TagIcon className="h-3 w-3" />
                        Filter by "{tagSearch.trim()}"
                      </button>
                    )}
                  </div>
                  <div className="max-h-64 overflow-y-auto p-1.5">
                    {tagsLoading && (
                      <div className="flex items-center justify-center py-6">
                        <Loader2 className="h-5 w-5 animate-spin text-fg/40" />
                      </div>
                    )}
                    {tagsError && !tagsLoading && (
                      <p className="px-2 py-3 text-xs text-danger">{tagsError}</p>
                    )}
                    {!tagsLoading && !tagsError && filteredAvailableTags.length === 0 && (
                      <p className="px-2 py-3 text-xs text-fg/40">
                        No matching tags in the loaded list. Type a tag above and press Enter to
                        filter by it anyway.
                      </p>
                    )}
                    {!tagsLoading &&
                      filteredAvailableTags.map((tag) => {
                        const selected = urlTags.includes(tag.name);
                        return (
                          <button
                            key={tag.name}
                            type="button"
                            onClick={() => toggleTag(tag.name)}
                            className={cn(
                              "flex w-full items-center justify-between rounded-lg px-2.5 py-1.5 text-left text-xs transition-colors",
                              selected
                                ? "bg-fg/15 text-fg"
                                : "text-fg/70 hover:bg-fg/10 hover:text-fg",
                            )}
                          >
                            <span className="flex items-center gap-2">
                              <span
                                className={cn(
                                  "flex h-3.5 w-3.5 items-center justify-center rounded border",
                                  selected
                                    ? "border-fg bg-fg text-surface"
                                    : "border-fg/30",
                                )}
                              >
                                {selected && (
                                  <svg viewBox="0 0 10 8" className="h-2 w-2 fill-current">
                                    <path d="M0.5 4 L3.5 7 L9.5 0.5" stroke="currentColor" strokeWidth="1.8" fill="none" />
                                  </svg>
                                )}
                              </span>
                              {tag.name}
                            </span>
                            {tag.count !== undefined && (
                              <span className="text-fg/35">
                                {tag.count.toLocaleString()}
                              </span>
                            )}
                          </button>
                        );
                      })}
                  </div>
                  {urlTags.length > 0 && (
                    <div className="border-t border-fg/10 p-2">
                      <button
                        type="button"
                        onClick={() => updateParams({ tags: [], page: 1 })}
                        className="w-full rounded-lg px-2 py-1.5 text-xs text-fg/50 transition-colors hover:bg-fg/10 hover:text-fg"
                      >
                        Clear all tags
                      </button>
                    </div>
                  )}
                </div>
              )}
            </div>

            {/* Selected tag chips */}
            {urlTags.map((tag) => (
              <span
                key={tag}
                className="flex h-9 items-center gap-1.5 rounded-xl border border-fg/20 bg-fg/10 px-3 text-xs font-medium text-fg"
              >
                {tag}
                <button
                  type="button"
                  onClick={() => toggleTag(tag)}
                  className="rounded-full p-0.5 text-fg/50 hover:bg-fg/20 hover:text-fg"
                  aria-label={`Remove tag ${tag}`}
                >
                  <X className="h-3 w-3" />
                </button>
              </span>
            ))}

            {/* Clear filters */}
            {hasActiveFilters && (
              <button
                type="button"
                onClick={clearFilters}
                className="h-9 rounded-xl px-3 text-xs font-medium text-fg/50 transition-colors hover:bg-fg/10 hover:text-fg"
              >
                Clear filters
              </button>
            )}
          </div>

          {/* Pure Mode notice — explains hidden NSFW cards */}
          {pureModeActive && (
            <div className="mt-3 flex flex-wrap items-center gap-2 rounded-xl border border-warning/25 bg-warning/10 px-3 py-2">
              <Shield className="h-3.5 w-3.5 shrink-0 text-warning" />
              <span className="text-xs text-fg/70">
                Pure Mode is on — NSFW characters are hidden, so Chub totals look smaller than the
                full catalog.
              </span>
              <button
                type="button"
                onClick={() => navigate("/settings/security")}
                className="ml-auto rounded-lg px-2 py-1 text-xs font-medium text-warning transition-colors hover:bg-warning/15"
              >
                Open Settings
              </button>
            </div>
          )}

          {/* Pagination + refresh */}
          <div className="mt-3 flex flex-wrap items-center gap-2">
            <div className="flex items-center gap-1">
              <button
                type="button"
                onClick={() => goToPage(1)}
                disabled={urlPage <= 1 || loading}
                className="flex h-9 w-9 items-center justify-center rounded-xl border border-fg/10 bg-fg/5 text-fg/70 transition-all hover:bg-fg/10 hover:text-fg disabled:cursor-not-allowed disabled:opacity-40"
                aria-label="First page"
                title="First page"
              >
                <svg viewBox="0 0 24 24" className="h-4 w-4" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
                  <polyline points="11 17 6 12 11 7" />
                  <polyline points="18 17 13 12 18 7" />
                </svg>
              </button>
              <button
                type="button"
                onClick={() => goToPage(urlPage - 1)}
                disabled={urlPage <= 1 || loading}
                className="flex h-9 w-9 items-center justify-center rounded-xl border border-fg/10 bg-fg/5 text-fg/70 transition-all hover:bg-fg/10 hover:text-fg disabled:cursor-not-allowed disabled:opacity-40"
                aria-label="Previous page"
              >
                <ChevronLeft className="h-4 w-4" />
              </button>
              <span className="min-w-16 text-center text-xs text-fg/50">
                {urlPage}
                {totalPages ? ` / ${totalPages}` : ""}
              </span>
              <button
                type="button"
                onClick={() => goToPage(urlPage + 1)}
                disabled={isLastPage}
                className="flex h-9 w-9 items-center justify-center rounded-xl border border-fg/10 bg-fg/5 text-fg/70 transition-all hover:bg-fg/10 hover:text-fg disabled:cursor-not-allowed disabled:opacity-40"
                aria-label="Next page"
              >
                <ChevronRight className="h-4 w-4" />
              </button>
            </div>

            {/* Live refresh from Chub */}
            <button
              type="button"
              onClick={handleRefresh}
              disabled={loading}
              title="Refresh from Chub AI"
              className={cn(
                "flex h-9 items-center gap-1.5 rounded-xl border border-fg/10 bg-fg/5 px-3",
                "text-sm font-medium text-fg/70 transition-all hover:bg-fg/10 hover:text-fg",
                "disabled:cursor-not-allowed disabled:opacity-50",
              )}
            >
              <RefreshCw className={cn("h-3.5 w-3.5", loading && "animate-spin")} />
              Refresh
            </button>

            {results?.totalResults !== undefined && (
              <span className="text-xs text-fg/35">
                {results.totalResults.toLocaleString()} characters
              </span>
            )}
          </div>
        </div>

        {/* Content */}
        <div className="px-4 pb-2 pt-4 lg:px-8">
          {/* Error */}
          {error && !loading && (
            <div className="flex flex-col items-center justify-center py-16">
              <div className="mb-4 flex h-16 w-16 items-center justify-center rounded-2xl border border-danger/30 bg-danger/10">
                <AlertCircle className="h-8 w-8 text-danger" />
              </div>
              <h3 className="mb-2 text-lg font-semibold text-fg">
                {t("discovery.errorTitle")}
              </h3>
              <p className="mb-6 text-center text-sm text-fg/50">{error}</p>
              <button
                type="button"
                onClick={() => setReloadToken((token) => token + 1)}
                className="flex items-center gap-2 rounded-xl border border-fg/20 bg-fg/10 px-5 py-2.5 text-sm font-medium text-fg transition-all hover:bg-fg/15 active:scale-95"
              >
                <RefreshCw className="h-4 w-4" />
                {t("common.buttons.retry")}
              </button>
            </div>
          )}

          {/* Initial loading skeleton */}
          {!error && loading && !results && (
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
              {Array.from({ length: DISCOVER_PAGE_SIZE }, (_, index) => (
                <SkeletonCard key={index} />
              ))}
            </div>
          )}

          {/* Grid (kept visible + dimmed while paging) */}
          {!error && results && (
            <div className="relative">
              <div
                className={cn(
                  "grid grid-cols-2 gap-3 transition-opacity sm:grid-cols-3",
                  softLoading && "pointer-events-none opacity-50",
                )}
              >
                {cards.map((character) => (
                  <DiscoverCharacterCard
                    key={character.id}
                    character={character}
                    showNsfw={showNsfw}
                    onClick={handleCardClick}
                  />
                ))}
              </div>

              {softLoading && (
                <div className="absolute inset-0 flex items-start justify-center pt-10">
                  <Loader2 className="h-6 w-6 animate-spin text-fg/60" />
                </div>
              )}

              {/* Empty state */}
              {cards.length === 0 && !softLoading && (
                <div className="flex flex-col items-center justify-center py-16">
                  <div className="mb-4 flex h-16 w-16 items-center justify-center rounded-2xl border border-fg/10 bg-fg/5">
                    <Search className="h-8 w-8 text-fg/30" />
                  </div>
                  <h3 className="mb-2 text-lg font-semibold text-fg">No characters found</h3>
                  <p className="text-center text-sm text-fg/50">
                    Try a different search or remove some tags.
                  </p>
                  {hasActiveFilters && (
                    <button
                      type="button"
                      onClick={clearFilters}
                      className="mt-4 rounded-xl border border-fg/20 bg-fg/10 px-4 py-2 text-sm font-medium text-fg transition-all hover:bg-fg/15 active:scale-95"
                    >
                      Clear filters
                    </button>
                  )}
                </div>
              )}
            </div>
          )}
        </div>
      </main>
    </div>
  );
}

export default DiscoveryPage;
