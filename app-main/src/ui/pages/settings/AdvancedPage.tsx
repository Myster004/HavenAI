import { useState, useEffect } from "react";
import { useNavigate } from "react-router-dom";
import {
  Sparkles,
  ChevronRight,
  PenLine,
  Brain,
  Cpu,
  Heart,
  MessageSquare,
  Zap,
  BookOpen,
  Network,
  Users,
  Lock,
} from "lucide-react";
import {
  readSettings,
  saveAdvancedSettings,
  checkEmbeddingModel,
  getHostApiStatus,
} from "../../../core/storage/repo";
import type { Settings } from "../../../core/storage/schemas";
import { cn, typography, spacing, interactive } from "../../design-tokens";
import { EmbeddingDownloadPrompt } from "../../components/EmbeddingDownloadPrompt";
import { useI18n } from "../../../core/i18n/context";
import { Switch } from "../../components/Switch";
import { NumberInput } from "../../components/NumberInput";

interface FeatureCardProps {
  title: string;
  description: string;
  detailText?: string;
  icon: React.ReactNode;
  enabled: boolean;
  onToggle: () => void;
  onNavigate: () => void;
  colorScheme: "rose" | "blue" | "emerald" | "amber" | "violet";
  badge?: string;
}

function FeatureCard({
  title,
  description,
  detailText,
  icon,
  enabled,
  onToggle,
  onNavigate,
  colorScheme,
  badge,
}: FeatureCardProps) {
  const colorStyles = {
    rose: {
      border: enabled ? "border-danger/25" : "border-fg/10",
      bg: enabled ? "bg-danger/8" : "bg-fg/5",
      hoverBorder: enabled ? "hover:border-danger/40" : "hover:border-fg/20",
      iconBorder: enabled ? "border-danger/40" : "border-fg/15",
      iconBg: enabled ? "bg-danger/15" : "bg-fg/8",
      iconShadow: enabled ? "shadow-danger/20" : "",
      iconColor: enabled ? "text-danger/80" : "text-fg/60",
      toggleBg: enabled ? "bg-danger" : "bg-fg/20",
      toggleShadow: enabled ? "shadow-danger/30" : "",
      badgeBorder: enabled ? "border-danger/50" : "border-fg/20",
      badgeBg: enabled ? "bg-danger/20" : "bg-fg/10",
      badgeText: enabled ? "text-danger" : "text-fg/60",
      gradient: enabled
        ? "radial-gradient(circle at 15% 15%, rgba(244,63,94,0.08) 0%, transparent 50%)"
        : "none",
    },
    blue: {
      border: enabled ? "border-info/25" : "border-fg/10",
      bg: enabled ? "bg-info/8" : "bg-fg/5",
      hoverBorder: enabled ? "hover:border-info/40" : "hover:border-fg/20",
      iconBorder: enabled ? "border-info/40" : "border-fg/15",
      iconBg: enabled ? "bg-info/15" : "bg-fg/8",
      iconShadow: enabled ? "shadow-info/20" : "",
      iconColor: enabled ? "text-info/80" : "text-fg/60",
      toggleBg: enabled ? "bg-info" : "bg-fg/20",
      toggleShadow: enabled ? "shadow-info/30" : "",
      badgeBorder: enabled ? "border-info/50" : "border-warning/40",
      badgeBg: enabled ? "bg-info/20" : "bg-warning/15",
      badgeText: enabled ? "text-info" : "text-warning",
      gradient: enabled
        ? "radial-gradient(circle at 15% 15%, rgba(59,130,246,0.06) 0%, transparent 50%)"
        : "none",
    },
    emerald: {
      border: enabled ? "border-accent/25" : "border-fg/10",
      bg: enabled ? "bg-accent/8" : "bg-fg/5",
      hoverBorder: enabled ? "hover:border-accent/40" : "hover:border-fg/20",
      iconBorder: enabled ? "border-accent/40" : "border-fg/15",
      iconBg: enabled ? "bg-accent/15" : "bg-fg/8",
      iconShadow: enabled ? "shadow-accent/20" : "",
      iconColor: enabled ? "text-accent/80" : "text-fg/60",
      toggleBg: enabled ? "bg-accent" : "bg-fg/20",
      toggleShadow: enabled ? "shadow-accent/30" : "",
      badgeBorder: enabled ? "border-accent/50" : "border-warning/40",
      badgeBg: enabled ? "bg-accent/20" : "bg-warning/15",
      badgeText: enabled ? "text-accent/80" : "text-warning",
      gradient: enabled
        ? "radial-gradient(circle at 15% 15%, rgba(16,185,129,0.08) 0%, transparent 50%)"
        : "none",
    },
    amber: {
      border: enabled ? "border-warning/25" : "border-fg/10",
      bg: enabled ? "bg-warning/8" : "bg-fg/5",
      hoverBorder: enabled ? "hover:border-warning/40" : "hover:border-fg/20",
      iconBorder: enabled ? "border-warning/40" : "border-fg/15",
      iconBg: enabled ? "bg-warning/15" : "bg-fg/8",
      iconShadow: enabled ? "shadow-warning/20" : "",
      iconColor: enabled ? "text-warning/80" : "text-fg/60",
      toggleBg: enabled ? "bg-warning" : "bg-fg/20",
      toggleShadow: enabled ? "shadow-warning/30" : "",
      badgeBorder: enabled ? "border-warning/50" : "border-fg/20",
      badgeBg: enabled ? "bg-warning/20" : "bg-fg/10",
      badgeText: enabled ? "text-warning/80" : "text-fg/60",
      gradient: enabled
        ? "radial-gradient(circle at 15% 15%, rgba(251,191,36,0.08) 0%, transparent 50%)"
        : "none",
    },
    violet: {
      border: enabled ? "border-secondary/25" : "border-fg/10",
      bg: enabled ? "bg-secondary/8" : "bg-fg/5",
      hoverBorder: enabled ? "hover:border-secondary/40" : "hover:border-fg/20",
      iconBorder: enabled ? "border-secondary/40" : "border-fg/15",
      iconBg: enabled ? "bg-secondary/15" : "bg-fg/8",
      iconShadow: enabled ? "shadow-secondary/20" : "",
      iconColor: enabled ? "text-secondary/80" : "text-fg/60",
      toggleBg: enabled ? "bg-secondary" : "bg-fg/20",
      toggleShadow: enabled ? "shadow-secondary/30" : "",
      badgeBorder: enabled ? "border-secondary/50" : "border-fg/20",
      badgeBg: enabled ? "bg-secondary/20" : "bg-fg/10",
      badgeText: enabled ? "text-secondary" : "text-fg/60",
      gradient: enabled
        ? "radial-gradient(circle at 15% 15%, rgba(139,92,246,0.08) 0%, transparent 50%)"
        : "none",
    },
  };

  const style = colorStyles[colorScheme];
  const toggleId = `toggle-${title.toLowerCase().replace(/\s+/g, "-")}`;

  return (
    <button
      onClick={onNavigate}
      className={cn(
        "group w-full text-left",
        "relative overflow-hidden rounded-xl border px-4 py-3.5",
        "transition-all duration-300",
        style.border,
        style.bg,
        style.hoverBorder,
        interactive.active.scale,
        interactive.focus.ring,
      )}
    >
      {enabled && (
        <div
          className="pointer-events-none absolute inset-0 opacity-70"
          style={{ background: style.gradient }}
        />
      )}

      <div className="relative flex items-start gap-3">
        <div
          className={cn(
            "flex h-9 w-9 shrink-0 items-center justify-center rounded-xl border",
            "transition-all duration-300",
            style.iconBorder,
            style.iconBg,
            enabled && "shadow-lg",
            style.iconShadow,
          )}
        >
          <span className={cn("transition-colors duration-300", style.iconColor)}>{icon}</span>
        </div>

        <div className="min-w-0 flex-1">
          <div className="flex items-center justify-between gap-2">
            <div className="flex-1">
              <div className="flex items-center gap-2">
                <span className={cn(typography.body.size, "font-medium text-fg")}>{title}</span>
                <span
                  className={cn(
                    "rounded-md border px-1.5 py-0.5",
                    "text-[9px] font-semibold leading-none uppercase tracking-[0.2em]",
                    "transition-all duration-300",
                    style.badgeBorder,
                    style.badgeBg,
                    style.badgeText,
                  )}
                >
                  {enabled ? "On" : "Off"}
                </span>
                {badge && (
                  <span
                    className={cn(
                      "rounded-md border px-1.5 py-0.5",
                      "text-[9px] font-medium leading-none uppercase tracking-wider",
                      "border-fg/10 bg-fg/5 text-fg/40",
                    )}
                  >
                    {badge}
                  </span>
                )}
              </div>
              <p className="mt-0.5 text-[11px] leading-relaxed text-fg/50">{description}</p>
            </div>

            <div className="flex shrink-0 items-center gap-2">
              <span onClick={(e) => e.stopPropagation()}>
                <Switch
                  id={toggleId}
                  checked={enabled}
                  onChange={() => onToggle()}
                />
              </span>
              <ChevronRight
                className={cn(
                  "h-4 w-4 shrink-0 text-fg/25 transition-colors",
                  "group-hover:text-fg/50",
                )}
              />
            </div>
          </div>

          {detailText && (
            <p className="mt-1.5 text-[10px] leading-relaxed text-fg/40">{detailText}</p>
          )}
        </div>
      </div>
    </button>
  );
}

interface SettingsSectionProps {
  title: string;
  children: React.ReactNode;
  icon?: React.ReactNode;
}

function SettingsSection({ title, children, icon }: SettingsSectionProps) {
  return (
    <div>
      <div className="mb-2.5 flex items-center gap-2 px-1">
        {icon && <span className="text-fg/30">{icon}</span>}
        <h2
          className={cn(
            typography.overline.size,
            typography.overline.weight,
            typography.overline.tracking,
            typography.overline.transform,
            "text-fg/40",
          )}
        >
          {title}
        </h2>
      </div>
      <div className="space-y-2.5">{children}</div>
    </div>
  );
}

export function AdvancedPage() {
  const navigate = useNavigate();
  const { t } = useI18n();
  const [isLoading, setIsLoading] = useState(true);
  const [showDownloadPrompt, setShowDownloadPrompt] = useState(false);
  const [chubApiKey, setChubApiKey] = useState("");

  // Settings state
  const [creationHelperEnabled, setCreationHelperEnabled] = useState(false);
  const [dynamicMemoryEnabled, setDynamicMemoryEnabled] = useState(false);
  const [helpMeReplyEnabled, setHelpMeReplyEnabled] = useState(true);
  const [manualWindow, setManualWindow] = useState<number | null>(50);
  const [hostApiEnabled, setHostApiEnabled] = useState(false);
  const [hostApiRunning, setHostApiRunning] = useState(false);

  const getAdvancedSettings = (settings: Settings) => {
    const advanced = settings.advancedSettings ?? {
      creationHelperEnabled: false,
      helpMeReplyEnabled: true,
    };
    if (advanced.creationHelperEnabled === undefined) {
      advanced.creationHelperEnabled = false;
    }
    if (advanced.helpMeReplyEnabled === undefined) {
      advanced.helpMeReplyEnabled = true;
    }
    settings.advancedSettings = advanced;
    return advanced;
  };

  useEffect(() => {
    const loadData = async () => {
      try {
        const settings = await readSettings();

        setCreationHelperEnabled(settings.advancedSettings?.creationHelperEnabled ?? false);
        setDynamicMemoryEnabled(settings.advancedSettings?.dynamicMemory?.enabled ?? false);
        setHelpMeReplyEnabled(settings.advancedSettings?.helpMeReplyEnabled ?? true);
        setManualWindow(settings.advancedSettings?.manualModeContextWindow ?? 50);
        setHostApiEnabled(settings.advancedSettings?.hostApi?.enabled ?? false);
        setChubApiKey(settings.advancedSettings?.chubApiKey ?? "");

        try {
          const status = await getHostApiStatus();
          setHostApiRunning(status.running);
        } catch {
          // ignore
        }

        setIsLoading(false);
      } catch (err) {
        console.error("Failed to load settings:", err);
        setIsLoading(false);
      }
    };

    loadData();
  }, []);

  const handleToggleCreationHelper = async () => {
    const newValue = !creationHelperEnabled;
    setCreationHelperEnabled(newValue);

    try {
      const settings = await readSettings();
      const advanced = getAdvancedSettings(settings);
      advanced.creationHelperEnabled = newValue;

      if (newValue && !advanced.creationHelperModelId && settings.defaultModelId) {
        advanced.creationHelperModelId = settings.defaultModelId;
      }

      await saveAdvancedSettings(advanced);
    } catch (err) {
      console.error("Failed to save creation helper setting:", err);
      setCreationHelperEnabled(!newValue);
    }
  };

  const handleToggleDynamicMemory = async () => {
    const newValue = !dynamicMemoryEnabled;

    if (newValue) {
      try {
        const modelExists = await checkEmbeddingModel();
        if (!modelExists) {
          setShowDownloadPrompt(true);
          return;
        }
      } catch (err) {
        console.error("Failed to check embedding model:", err);
        return;
      }
    }

    setDynamicMemoryEnabled(newValue);

    try {
      const settings = await readSettings();
      const advanced = getAdvancedSettings(settings);
      if (!advanced.dynamicMemory) {
        advanced.dynamicMemory = {
          enabled: false,
          summaryMessageInterval: 20,
          maxEntries: 50,
          minSimilarityThreshold: 0.35,
          retrievalLimit: 5,
          retrievalStrategy: "smart",
          hotMemoryTokenBudget: 2000,
          decayRate: 0.08,
          coldThreshold: 0.3,
          deleteConfidenceDefault: 0.5,
          maxHardDeleteRatioPerCycle: 0.5,
          contextEnrichmentEnabled: true,
          recursiveMemoryLoops: false,
          recursiveMemoryLoopHardCap: 20,
        };
      }

      if (newValue && !advanced.summarisationModelId && settings.defaultModelId) {
        advanced.summarisationModelId = settings.defaultModelId;
      }

      if (advanced.dynamicMemory) {
        advanced.dynamicMemory.enabled = newValue;
      }
      await saveAdvancedSettings(advanced);
    } catch (err) {
      console.error("Failed to save dynamic memory setting:", err);
      setDynamicMemoryEnabled(!newValue);
    }
  };

  const handleManualWindowChange = async (value: number | null) => {
    setManualWindow(value);

    try {
      const settings = await readSettings();
      const advanced = getAdvancedSettings(settings);
      advanced.manualModeContextWindow = value === null ? 50 : value;
      await saveAdvancedSettings(advanced);
    } catch (err) {
      console.error("Failed to save manual window setting:", err);
    }
  };


  const handleToggleHelpMeReply = async () => {
    const newValue = !helpMeReplyEnabled;
    setHelpMeReplyEnabled(newValue);

    try {
      const settings = await readSettings();
      const advanced = getAdvancedSettings(settings);
      advanced.helpMeReplyEnabled = newValue;
      await saveAdvancedSettings(advanced);
    } catch (err) {
      console.error("Failed to save help me reply setting:", err);
      setHelpMeReplyEnabled(!newValue);
    }
  };

  const handleToggleHostApi = async () => {
    const newValue = !hostApiEnabled;
    setHostApiEnabled(newValue);
    try {
      const settings = await readSettings();
      const advanced = getAdvancedSettings(settings);
      if (!advanced.hostApi) {
        advanced.hostApi = {
          enabled: false,
          bindAddress: "0.0.0.0",
          port: 3333,
          token: "",
          exposedModels: [],
        };
      }
      advanced.hostApi.enabled = newValue;
      await saveAdvancedSettings(advanced);
    } catch (err) {
      console.error("Failed to save host API setting:", err);
      setHostApiEnabled(!newValue);
    }
  };

  const handleChubApiKeyCommit = async () => {
    const trimmed = chubApiKey.trim();
    try {
      const settings = await readSettings();
      const advanced = getAdvancedSettings(settings);
      const current = advanced.chubApiKey ?? "";
      if (current === trimmed) return;
      if (trimmed) {
        advanced.chubApiKey = trimmed;
      } else {
        delete advanced.chubApiKey;
      }
      await saveAdvancedSettings(advanced);
    } catch (err) {
      console.error("Failed to save Chub API key:", err);
    }
  };

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="h-6 w-6 animate-spin rounded-full border-2 border-fg/20 border-t-fg/60" />
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      <section className={cn("flex-1 overflow-y-auto px-3 pt-3 pb-6", spacing.section)}>
        {/* AI Features Section */}
        <SettingsSection title={t("advanced.sectionTitles.aiFeatures")} icon={<Zap size={12} />}>
          <FeatureCard
            title={t("advanced.creationHelper.title")}
            description={t("advanced.creationHelper.description")}
            detailText="Get intelligent suggestions for personality traits, backstory, and dialogue style"
            icon={<Sparkles className="h-4 w-4" />}
            enabled={creationHelperEnabled}
            onToggle={handleToggleCreationHelper}
            onNavigate={() => navigate("/settings/advanced/creation-helper")}
            colorScheme="rose"
          />

          <FeatureCard
            title={t("advanced.helpMeReply.title")}
            description={t("advanced.helpMeReply.description")}
            detailText="Generate contextual response options based on conversation history"
            icon={<PenLine className="h-4 w-4" />}
            enabled={helpMeReplyEnabled}
            onToggle={handleToggleHelpMeReply}
            onNavigate={() => navigate("/settings/advanced/help-me-reply")}
            colorScheme="emerald"
          />

          <button
            type="button"
            onClick={() => navigate("/settings/advanced/lorebooks")}
            className={cn(
              "group w-full text-left",
              "relative overflow-hidden rounded-xl border border-fg/10 bg-fg/5 px-4 py-3.5",
              "transition-all duration-300 hover:border-fg/20",
              interactive.active.scale,
              interactive.focus.ring,
            )}
          >
            <div className="relative flex items-start gap-3">
              <div
                className={cn(
                  "flex h-9 w-9 shrink-0 items-center justify-center rounded-xl border",
                  "border-accent/30 bg-accent/10 text-accent/90",
                )}
              >
                <BookOpen className="h-4 w-4" />
              </div>

              <div className="min-w-0 flex-1">
                <div className="flex items-center justify-between gap-2">
                  <div className="flex-1">
                    <span className={cn(typography.body.size, "font-medium text-fg")}>
                      Lorebooks
                    </span>
                    <p className="mt-0.5 text-[11px] leading-relaxed text-fg/50">
                      Configure the full lorebook generator pipeline and the entry generator that
                      drafts single entries and keywords from chat messages.
                    </p>
                  </div>

                  <ChevronRight className="h-4 w-4 shrink-0 text-fg/25 transition-colors group-hover:text-fg/50" />
                </div>
              </div>
            </div>
          </button>

          <button
            type="button"
            onClick={() => navigate("/settings/advanced/group-chats")}
            className={cn(
              "group w-full text-left",
              "relative overflow-hidden rounded-xl border border-fg/10 bg-fg/5 px-4 py-3.5",
              "transition-all duration-300 hover:border-fg/20",
              interactive.active.scale,
              interactive.focus.ring,
            )}
          >
            <div className="relative flex items-start gap-3">
              <div
                className={cn(
                  "flex h-9 w-9 shrink-0 items-center justify-center rounded-xl border",
                  "border-info/30 bg-info/10 text-info/90",
                )}
              >
                <Users className="h-4 w-4" />
              </div>

              <div className="min-w-0 flex-1">
                <div className="flex items-center justify-between gap-2">
                  <div className="flex-1">
                    <span className={cn(typography.body.size, "font-medium text-fg")}>
                      {t("advanced.groupChats.title")}
                    </span>
                    <p className="mt-0.5 text-[11px] leading-relaxed text-fg/50">
                      {t("advanced.groupChats.description")}
                    </p>
                  </div>

                  <ChevronRight className="h-4 w-4 shrink-0 text-fg/25 transition-colors group-hover:text-fg/50" />
                </div>
              </div>
            </div>
          </button>

        </SettingsSection>

        {/* Memory System Section */}
        <SettingsSection
          title={t("advanced.sectionTitles.memorySystem")}
          icon={<Brain size={12} />}
        >
          <FeatureCard
            title={t("advanced.dynamicMemory.title")}
            description={
              dynamicMemoryEnabled
                ? "AI automatically manages conversation context"
                : "Switch to automatic memory management"
            }
            detailText="Semantic search enables intelligent memory recall across conversations"
            icon={<Cpu className="h-4 w-4" />}
            enabled={dynamicMemoryEnabled}
            onToggle={handleToggleDynamicMemory}
            onNavigate={() => navigate("/settings/advanced/memory")}
            colorScheme="blue"
          />

          {/* Context Window Settings Card */}
          <div className={cn("rounded-xl border px-4 py-4", "border-fg/10 bg-fg/5")}>
            <div className="flex items-start gap-3">
              <div
                className={cn(
                  "flex h-9 w-9 shrink-0 items-center justify-center rounded-xl border",
                  "border-fg/10 bg-fg/5",
                )}
              >
                <MessageSquare className="h-4 w-4 text-fg/50" />
              </div>

              <div className="min-w-0 flex-1">
                <div className="flex items-center justify-between">
                  <div>
                    <span className={cn(typography.body.size, "font-medium text-fg")}>
                      {t("advanced.dynamicMemory.contextWindow")}
                    </span>
                    <p className="mt-0.5 text-[11px] text-fg/45">
                      {t("advanced.dynamicMemory.contextWindowDesc")}
                    </p>
                  </div>
                  <NumberInput
                    min={1}
                    max={1000}
                    value={manualWindow ?? 50}
                    onChange={(next) => handleManualWindowChange(next)}
                    className={cn(
                      "w-20 rounded-lg border border-fg/15 bg-surface-el/30 px-3 py-1.5",
                      "text-center font-mono text-sm text-fg",
                      "focus:border-fg/30 focus:outline-none",
                      interactive.transition.fast,
                    )}
                  />
                </div>
              </div>
            </div>
          </div>
        </SettingsSection>

        {/* Companions Section */}
        <SettingsSection title="Companions" icon={<Heart size={12} />}>
          <button
            type="button"
            onClick={() => navigate("/settings/advanced/companions")}
            className={cn(
              "group w-full text-left",
              "relative overflow-hidden rounded-xl border border-fg/10 bg-fg/5 px-4 py-3.5",
              "transition-all duration-300 hover:border-fg/20",
              interactive.active.scale,
              interactive.focus.ring,
            )}
          >
            <div className="relative flex items-start gap-3">
              <div
                className={cn(
                  "flex h-9 w-9 shrink-0 items-center justify-center rounded-xl border",
                  "border-rose-400/30 bg-rose-400/10 text-rose-300",
                )}
              >
                <Heart className="h-4 w-4" />
              </div>

              <div className="min-w-0 flex-1">
                <div className="flex items-center justify-between gap-2">
                  <div className="flex-1">
                    <span className={cn(typography.body.size, "font-medium text-fg")}>
                      Companions
                    </span>
                    <p className="mt-0.5 text-[11px] leading-relaxed text-fg/50">
                      Manage local analysis models for emotion, entity, and memory routing, and
                      configure the Companion Soul Writer that drafts persona souls.
                    </p>
                  </div>

                  <ChevronRight className="h-4 w-4 shrink-0 text-fg/25 transition-colors group-hover:text-fg/50" />
                </div>
              </div>
            </div>
          </button>
        </SettingsSection>

        {/* Network Section */}
        <SettingsSection title="Network" icon={<Network size={12} />}>
          <FeatureCard
            title="API Server"
            description="Expose models via an OpenAI-compatible API server"
            detailText={hostApiRunning ? "Server is currently running" : undefined}
            icon={<Network className="h-4 w-4" />}
            enabled={hostApiEnabled}
            onToggle={handleToggleHostApi}
            onNavigate={() => navigate("/settings/advanced/host-api")}
            colorScheme="blue"
          />
        </SettingsSection>

        {/* Chub API Key for full catalog access */}
        <SettingsSection title="Chub API Key" icon={<Lock size={12} />}>
          <p className="mt-0.5 text-[11px] leading-relaxed text-fg/50">
            Add your Chub API key to access the full live catalog. Without a key, the public anonymous catalog is used.
          </p>
          <div className="mt-2 flex flex-col gap-2">
            <label
              className={cn(
                "flex items-center text-fg/60 text-[11px]",
                "hover:text-fg",
                "focus-within:text-fg",
              )}
            >
              <span className="w-20">Chub API Key:</span>
              <input
                type="password"
                value={chubApiKey}
                onChange={(e) => setChubApiKey(e.target.value)}
                onBlur={handleChubApiKeyCommit}
                maxLength={128}
                className={cn(
                  "w-full mt-1 rounded-lg border border-fg/15 bg-surface-el/30 px-3 py-1.5",
                  "text-sm text-fg",
                  "focus:border-fg/30 focus:outline-none",
                  interactive.transition.fast,
                )}
                placeholder="Paste your Chub API key (optional)"
                disabled={isLoading}
              />
            </label>
            <p className="text-[10px] text-fg/40">
              This key is used only for Chub API catalog access. It is not stored or transmitted outside this app.
            </p>
            <div className="mt-3 rounded-lg border border-fg/10 bg-fg/[0.03] px-3 py-2.5">
              <p className="text-[11px] font-medium text-fg/70">How to get your Chub API key:</p>
              <ol className="mt-1.5 list-decimal space-y-1 pl-5 text-[11px] leading-relaxed text-fg/60">
                <li>Open chub.ai in a desktop browser (Chrome, Firefox, Edge, etc.) and log in.</li>
                <li>Press F12 (or right-click → Inspect) to open Developer Tools.</li>
                <li>
                  In the Console tab, paste this and press Enter:
                  <code className="mt-1 block max-w-full overflow-x-auto whitespace-pre-wrap break-all rounded bg-surface-el px-2 py-1.5 font-mono text-[11px] text-fg/80">
                    console.log(localStorage.getItem(&apos;URQL_TOKEN&apos;))
                  </code>
                </li>
              </ol>
            </div>
          </div>
        </SettingsSection>

      </section>

      <EmbeddingDownloadPrompt
        isOpen={showDownloadPrompt}
        onClose={() => setShowDownloadPrompt(false)}
      />
    </div>
  );
}
