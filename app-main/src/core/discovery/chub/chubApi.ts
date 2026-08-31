import { invoke } from "@tauri-apps/api/core";
import {
  DISCOVER_PAGE_SIZE,
  type DiscoverSearchResult,
  type DiscoverSort,
  type DiscoverTag,
  type ChubCharacterDetail,
} from "./chubTypes";
import { mapChubDetailNode, mapChubNodeToDiscoverCharacter } from "./chubMapper";
import { readAdvancedSettings } from "../../storage/advanced";

/**
 * Chub provider — the ONLY module allowed to talk to the Chub backend.
 * The UI consumes the normalized models from chubTypes.ts exclusively.
 */

interface ChubSearchResponseDto {
  nodes: unknown[];
  page: number;
  totalPages?: number;
  totalNodes?: number;
}

export async function searchDiscoverCharacters(options: {
  query?: string;
  tags: string[];
  page: number;
  sort: DiscoverSort;
  refresh?: boolean;
  apiKey?: string;
}): Promise<DiscoverSearchResult> {
  const advancedSettings = await readAdvancedSettings();
  const apiKey = advancedSettings.chubApiKey ?? options.apiKey;

  const dto = await invoke<ChubSearchResponseDto>("chub_search_characters", {
    query: options.query?.trim() ? options.query.trim() : null,
    tags: options.tags.length > 0 ? options.tags : null,
    page: options.page,
    pageSize: DISCOVER_PAGE_SIZE,
    sort: options.sort,
    bypassCache: options.refresh ?? false,
    apiKey: apiKey && apiKey.trim() ? apiKey : null,
  });

  const totalPages =
    dto.totalPages ??
    (dto.totalNodes && dto.totalNodes > 0
      ? Math.ceil(dto.totalNodes / DISCOVER_PAGE_SIZE)
      : undefined);

  return {
    characters: (dto.nodes ?? []).map(mapChubNodeToDiscoverCharacter),
    page: dto.page ?? options.page,
    totalPages,
    totalResults: dto.totalNodes,
  };
}

// ---------------------------------------------------------------------------
// Tag list (cached in-memory, TTL 10 minutes)
// ---------------------------------------------------------------------------

const TAG_CACHE_TTL_MS = 10 * 60 * 1000;
const tagCache = new Map<string, { at: number; tags: DiscoverTag[] }>();

export async function fetchDiscoverTags(
  search = "",
  refresh = false,
): Promise<DiscoverTag[]> {
  const key = search.trim().toLowerCase();
  if (refresh) tagCache.delete(key);
  const cached = tagCache.get(key);
  if (cached && Date.now() - cached.at < TAG_CACHE_TTL_MS) {
    return cached.tags;
  }

  const tags = await invoke<{ name: string; count?: number | null }[]>(
    "chub_fetch_tags",
    {
      search: key ? key : null,
      limit: 250,
      bypassCache: refresh,
    },
  );

  const mapped: DiscoverTag[] = (tags ?? []).map((tag) => ({
    name: tag.name,
    count: tag.count ?? undefined,
  }));

  tagCache.set(key, { at: Date.now(), tags: mapped });
  return mapped;
}

export function clearDiscoverTagCache() {
  tagCache.clear();
}

// ---------------------------------------------------------------------------
// Detail / import
// ---------------------------------------------------------------------------

export async function fetchDiscoverCharacterDetail(
  fullPath: string,
  apiKey?: string,
): Promise<ChubCharacterDetail> {
  const advancedSettings = await readAdvancedSettings();
  const key = apiKey ?? advancedSettings.chubApiKey;

  const node = await invoke<unknown>("chub_character_detail", {
    fullPath,
    apiKey: key && key.trim() ? key : null,
  });
  return mapChubDetailNode(node);
}

/** Returns the local character id when this Chub character was already downloaded. */
export async function getDiscoverImportStatus(
  fullPath: string,
): Promise<string | null> {
  const result = await invoke<string | null>("chub_import_status", { fullPath });
  return result ?? null;
}

/** Downloads + imports the character into local storage (Chats + Library). */
export async function importDiscoverCharacter(fullPath: string, apiKey?: string): Promise<string> {
  const advancedSettings = await readAdvancedSettings();
  const key = apiKey ?? advancedSettings.chubApiKey;
  return invoke<string>("chub_import_character", {
    fullPath,
    apiKey: key && key.trim() ? key : null,
  });
}

/** Format number with K/M suffix for display. */
export function formatCount(num?: number | null): string {
  if (num === undefined || num === null) return "0";
  if (num >= 1_000_000) return `${(num / 1_000_000).toFixed(1)}M`;
  if (num >= 1_000) return `${(num / 1_000).toFixed(1)}K`;
  return String(num);
}
