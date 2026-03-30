import { useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { Checkbox } from '@vibe/ui/components/Checkbox';
import { Input } from '@vibe/ui/components/Input';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@vibe/ui/components/Select';
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@vibe/ui/components/RadixTooltip';
import { InfoIcon } from 'lucide-react';

// ---------- Types ----------

type AgentId = 'claude_code' | 'codex' | 'gemini';
type ApprovalMode = 'auto' | 'approval';
type ReviewerMode = 'consensus' | 'strict';

interface StageConfig {
  agent: AgentId;
  approval: ApprovalMode;
  reviewer_mode?: ReviewerMode;
  second_agent?: AgentId | null;
  second_agent_after_attempts?: number;
}

export interface PipelineConfig {
  template: string;
  stages: Record<string, StageConfig>;
  create_pr: boolean;
}

interface PipelineSettingsProps {
  value: PipelineConfig | null;
  onChange: (config: PipelineConfig | null) => void;
}

// ---------- Constants ----------

const STAGE_KEYS = [
  'planner',
  'reviewer',
  'builder',
  'codeReview',
  'tester',
] as const;

const AGENTS: { value: AgentId; label: string }[] = [
  { value: 'claude_code', label: 'Claude Code' },
  { value: 'codex', label: 'Codex' },
  { value: 'gemini', label: 'Gemini' },
];

function getDefaultConfig(): PipelineConfig {
  return {
    template: 'full_pipeline',
    stages: {
      planner: { agent: 'claude_code', approval: 'auto' },
      reviewer: {
        agent: 'claude_code',
        approval: 'approval',
        reviewer_mode: 'consensus',
      },
      builder: { agent: 'claude_code', approval: 'auto' },
      codeReview: {
        agent: 'claude_code',
        approval: 'auto',
        reviewer_mode: 'strict',
      },
      tester: { agent: 'claude_code', approval: 'auto' },
    },
    create_pr: true,
  };
}

// ---------- Helper: Info tooltip ----------

function InfoTooltip({ text }: { text: string }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <InfoIcon className="inline-block h-3.5 w-3.5 text-muted-foreground cursor-help ml-1 shrink-0" />
      </TooltipTrigger>
      <TooltipContent className="max-w-[260px]">
        <p className="text-xs">{text}</p>
      </TooltipContent>
    </Tooltip>
  );
}

// ---------- Main Component ----------

export function PipelineSettings({ value, onChange }: PipelineSettingsProps) {
  const { t } = useTranslation('pipeline');
  const enabled = value !== null;
  const config = value ?? getDefaultConfig();

  const handleToggle = useCallback(
    (checked: boolean) => {
      onChange(checked ? getDefaultConfig() : null);
    },
    [onChange]
  );

  const updateStage = useCallback(
    (stageKey: string, patch: Partial<StageConfig>) => {
      if (!value) return;
      onChange({
        ...value,
        stages: {
          ...value.stages,
          [stageKey]: { ...value.stages[stageKey]!, ...patch },
        },
      });
    },
    [value, onChange]
  );

  const toggleSecondAgent = useCallback(
    (stageKey: string, checked: boolean) => {
      if (!value) return;
      if (checked) {
        updateStage(stageKey, {
          second_agent: 'gemini',
          second_agent_after_attempts: 3,
        });
      } else {
        updateStage(stageKey, {
          second_agent: null,
          second_agent_after_attempts: undefined,
        });
      }
    },
    [value, updateStage]
  );

  return (
    <TooltipProvider>
      <div className="flex flex-col gap-base">
        {/* Pipeline toggle */}
        <div className="flex items-center gap-half">
          <Checkbox
            id="pipeline-toggle"
            checked={enabled}
            onCheckedChange={handleToggle}
          />
          <label
            htmlFor="pipeline-toggle"
            className="text-sm font-medium text-normal cursor-pointer select-none"
          >
            {t('toggle')}
          </label>
          <InfoTooltip text={t('tooltips.pipeline')} />
        </div>

        {enabled && (
          <div className="flex flex-col gap-base rounded-sm border border-border/60 p-base">
            {/* Template selector */}
            <div className="flex items-center gap-half">
              <label className="text-xs text-low w-20 shrink-0">
                {t('template')}
              </label>
              <Select
                value={config.template}
                onValueChange={(v) => onChange({ ...config, template: v })}
              >
                <SelectTrigger className="h-8 text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="full_pipeline">
                    {t('fullPipeline')}
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>

            {/* Stages table */}
            <div className="overflow-x-auto">
              <table className="w-full text-xs">
                <thead>
                  <tr className="border-b border-border/40">
                    <th className="py-1 pr-2 text-left text-low font-medium">
                      Stage
                    </th>
                    <th className="py-1 px-2 text-left text-low font-medium">
                      {t('agent')}
                    </th>
                    <th className="py-1 px-2 text-left text-low font-medium">
                      {t('approval')}
                    </th>
                    <th className="py-1 pl-2 text-left text-low font-medium">
                      {t('secondAgent')}
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {STAGE_KEYS.map((stageKey) => {
                    const stage = config.stages[stageKey];
                    if (!stage) return null;
                    const isReviewerStage =
                      stageKey === 'reviewer' || stageKey === 'codeReview';
                    const hasSecondAgent = !!stage.second_agent;

                    return (
                      <tr
                        key={stageKey}
                        className="border-b border-border/20 last:border-b-0"
                      >
                        {/* Stage name */}
                        <td className="py-1.5 pr-2">
                          <div className="flex items-center">
                            <span className="text-normal font-medium">
                              {t(`stages.${stageKey}`)}
                            </span>
                            <InfoTooltip text={t(`tooltips.${stageKey}`)} />
                          </div>
                        </td>

                        {/* Agent selector */}
                        <td className="py-1.5 px-2">
                          <Select
                            value={stage.agent}
                            onValueChange={(v) =>
                              updateStage(stageKey, {
                                agent: v as AgentId,
                              })
                            }
                          >
                            <SelectTrigger className="h-7 text-xs min-w-[110px]">
                              <SelectValue />
                            </SelectTrigger>
                            <SelectContent>
                              {AGENTS.map((a) => (
                                <SelectItem key={a.value} value={a.value}>
                                  {a.label}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        </td>

                        {/* Approval mode */}
                        <td className="py-1.5 px-2">
                          <div className="flex flex-col gap-1">
                            <Select
                              value={stage.approval}
                              onValueChange={(v) =>
                                updateStage(stageKey, {
                                  approval: v as ApprovalMode,
                                })
                              }
                            >
                              <SelectTrigger className="h-7 text-xs min-w-[100px]">
                                <SelectValue />
                              </SelectTrigger>
                              <SelectContent>
                                <SelectItem value="auto">
                                  {t('auto')}
                                </SelectItem>
                                <SelectItem value="approval">
                                  {t('approvalRequired')}
                                </SelectItem>
                              </SelectContent>
                            </Select>
                            {isReviewerStage && stage.reviewer_mode && (
                              <div className="flex items-center gap-1">
                                <span className="text-low text-[10px]">
                                  {t('reviewMode')}:
                                </span>
                                <Select
                                  value={stage.reviewer_mode}
                                  onValueChange={(v) =>
                                    updateStage(stageKey, {
                                      reviewer_mode: v as ReviewerMode,
                                    })
                                  }
                                >
                                  <SelectTrigger className="h-6 text-[10px] min-w-[90px]">
                                    <SelectValue />
                                  </SelectTrigger>
                                  <SelectContent>
                                    <SelectItem value="consensus">
                                      {t('consensus')}
                                      <InfoTooltip
                                        text={t('tooltips.consensus')}
                                      />
                                    </SelectItem>
                                    <SelectItem value="strict">
                                      {t('strict')}
                                      <InfoTooltip
                                        text={t('tooltips.strict')}
                                      />
                                    </SelectItem>
                                  </SelectContent>
                                </Select>
                              </div>
                            )}
                          </div>
                        </td>

                        {/* Second agent */}
                        <td className="py-1.5 pl-2">
                          <div className="flex flex-col gap-1">
                            <div className="flex items-center gap-1">
                              <Checkbox
                                checked={hasSecondAgent}
                                onCheckedChange={(checked) =>
                                  toggleSecondAgent(stageKey, checked)
                                }
                              />
                              <InfoTooltip text={t('tooltips.secondAgent')} />
                            </div>
                            {hasSecondAgent && (
                              <div className="flex items-center gap-1">
                                <Select
                                  value={stage.second_agent ?? 'gemini'}
                                  onValueChange={(v) =>
                                    updateStage(stageKey, {
                                      second_agent: v as AgentId,
                                    })
                                  }
                                >
                                  <SelectTrigger className="h-6 text-[10px] min-w-[90px]">
                                    <SelectValue />
                                  </SelectTrigger>
                                  <SelectContent>
                                    {AGENTS.map((a) => (
                                      <SelectItem key={a.value} value={a.value}>
                                        {a.label}
                                      </SelectItem>
                                    ))}
                                  </SelectContent>
                                </Select>
                                <span className="text-low text-[10px] whitespace-nowrap">
                                  {t('afterAttempts')}
                                </span>
                                <Input
                                  type="number"
                                  min={1}
                                  max={10}
                                  value={stage.second_agent_after_attempts ?? 3}
                                  onChange={(e) =>
                                    updateStage(stageKey, {
                                      second_agent_after_attempts:
                                        parseInt(e.target.value) || 3,
                                    })
                                  }
                                  className="h-6 w-12 text-[10px] px-1"
                                />
                              </div>
                            )}
                          </div>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>

            {/* Create PR checkbox */}
            <div className="flex items-center gap-half pt-half">
              <Checkbox
                id="pipeline-create-pr"
                checked={config.create_pr}
                onCheckedChange={(checked) =>
                  onChange({ ...config, create_pr: checked })
                }
              />
              <label
                htmlFor="pipeline-create-pr"
                className="text-xs text-normal cursor-pointer select-none"
              >
                {t('createPr')}
              </label>
              <InfoTooltip text={t('tooltips.createPr')} />
            </div>
          </div>
        )}
      </div>
    </TooltipProvider>
  );
}
