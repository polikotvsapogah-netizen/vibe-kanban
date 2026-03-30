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

// ---------- Backend-compatible Types ----------

type AgentId = 'claude_code' | 'codex' | 'gemini';
type ApprovalMode = 'auto' | 'approval';

interface StageConfig {
  id: string;
  role: string;
  agent: string;
  approval: string;
  on_success: string;
  on_fail: string;
  max_retries: number | null;
  escalate_agent?: string | null;
  escalate_after_retries?: number | null;
  workflow_profile?: string | null;
  workflow_mode?: string | null;
  policies: string[];
}

export interface PipelineConfig {
  name: string;
  enable_second_agent: boolean;
  default_max_retries: number;
  auto_create_pr: boolean;
  stages: StageConfig[];
}

interface PipelineSettingsProps {
  value: PipelineConfig | null;
  onChange: (config: PipelineConfig | null) => void;
}

// ---------- Constants ----------

/** UI stage keys in display order, matching stage ids */
const STAGE_KEYS = [
  'planner',
  'reviewer',
  'builder',
  'code_reviewer',
  'tester',
  'finisher',
] as const;

/** Map from stage id to the translation key used in the UI */
const STAGE_I18N_KEY: Record<string, string> = {
  planner: 'planner',
  reviewer: 'reviewer',
  builder: 'builder',
  code_reviewer: 'codeReview',
  tester: 'tester',
  finisher: 'finisher',
};

const AGENTS: { value: AgentId; label: string }[] = [
  { value: 'claude_code', label: 'Claude Code' },
  { value: 'codex', label: 'Codex' },
  { value: 'gemini', label: 'Gemini' },
];

function getDefaultConfig(): PipelineConfig {
  return {
    name: 'Full Pipeline',
    enable_second_agent: false,
    default_max_retries: 7,
    auto_create_pr: true,
    stages: [
      {
        id: 'planner',
        role: 'planner',
        agent: 'claude_code',
        approval: 'auto',
        on_success: 'reviewer',
        on_fail: 'pause',
        max_retries: null,
        workflow_profile: 'brainstorming',
        workflow_mode: 'consensus',
        policies: ['structured_verdict', 'structured_handoff'],
      },
      {
        id: 'reviewer',
        role: 'reviewer',
        agent: 'claude_code',
        approval: 'approval',
        on_success: 'builder',
        on_fail: 'planner',
        max_retries: 7,
        workflow_profile: 'brainstorming',
        workflow_mode: 'consensus',
        policies: ['structured_verdict', 'structured_handoff'],
      },
      {
        id: 'builder',
        role: 'builder',
        agent: 'claude_code',
        approval: 'auto',
        on_success: 'code_reviewer',
        on_fail: 'pause',
        max_retries: null,
        workflow_profile: 'executing-plans',
        workflow_mode: 'batched',
        policies: ['structured_verdict', 'structured_handoff'],
      },
      {
        id: 'code_reviewer',
        role: 'code_reviewer',
        agent: 'claude_code',
        approval: 'auto',
        on_success: 'tester',
        on_fail: 'builder',
        max_retries: 3,
        workflow_profile: 'requesting-code-review',
        workflow_mode: 'strict',
        policies: ['structured_verdict', 'structured_handoff'],
      },
      {
        id: 'tester',
        role: 'tester',
        agent: 'claude_code',
        approval: 'auto',
        on_success: 'finisher',
        on_fail: 'code_reviewer',
        max_retries: 3,
        escalate_agent: 'codex',
        escalate_after_retries: 3,
        workflow_profile: 'verification-before-completion',
        workflow_mode: 'strict',
        policies: [
          'structured_verdict',
          'structured_handoff',
          'require_tests',
          'no_done_without_verification',
        ],
      },
      {
        id: 'finisher',
        role: 'finisher',
        agent: 'claude_code',
        approval: 'auto',
        on_success: 'complete',
        on_fail: 'tester',
        max_retries: 2,
        workflow_profile: 'finishing-a-development-branch',
        workflow_mode: null,
        policies: ['structured_verdict', 'no_done_without_verification'],
      },
    ],
  };
}

// ---------- Helpers ----------

function findStage(
  config: PipelineConfig,
  stageId: string
): StageConfig | undefined {
  return config.stages.find((s) => s.id === stageId);
}

function updateStageInConfig(
  config: PipelineConfig,
  stageId: string,
  patch: Partial<StageConfig>
): PipelineConfig {
  return {
    ...config,
    stages: config.stages.map((s) =>
      s.id === stageId ? { ...s, ...patch } : s
    ),
  };
}

function isReviewerStage(stageId: string): boolean {
  return stageId === 'reviewer' || stageId === 'code_reviewer';
}

// ---------- Helper: Info tooltip ----------

// Russian tooltips — always shown regardless of UI language
const RU_TOOLTIPS: Record<string, string> = {
  pipeline:
    'Включить многоэтапный пайплайн. Задача проходит через несколько AI-агентов: планирование, ревью, написание кода, тестирование',
  planner: 'Создаёт детальный план реализации на основе описания задачи',
  reviewer:
    'Проверяет план на качество и полноту. Возвращает на доработку при обнаружении проблем',
  builder:
    'Пишет код по утверждённому плану. Также исправляет баги, найденные при код-ревью',
  codeReview:
    'Проверяет написанный код на баги, ошибки и соответствие плану',
  tester:
    'Планирует тесты на основе спецификации и архитектуры, пишет и запускает их',
  finisher:
    'Проверяет прохождение всех тестов, подготавливает ветку к мержу, создаёт Pull Request',
  consensus:
    'Ревьюер может переписать и улучшить план. Агенты согласовывают за несколько раундов',
  strict: 'Ревьюер только критикует, не переписывает план',
  secondAgent:
    'Когда основной агент не справляется после повторных попыток, задача передаётся второму агенту с полным контекстом',
  createPr:
    'После прохождения всех этапов автоматически создать Pull Request',
};

function InfoTooltip({ text, ruKey }: { text: string; ruKey?: string }) {
  const tooltip = ruKey ? RU_TOOLTIPS[ruKey] || text : text;
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <InfoIcon className="inline-block h-3.5 w-3.5 text-green-500 cursor-help ml-1 shrink-0" />
      </TooltipTrigger>
      <TooltipContent className="max-w-[280px]">
        <p className="text-xs">{tooltip}</p>
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

  const handleStageUpdate = useCallback(
    (stageId: string, patch: Partial<StageConfig>) => {
      if (!value) return;
      onChange(updateStageInConfig(value, stageId, patch));
    },
    [value, onChange]
  );

  const toggleSecondAgent = useCallback(
    (checked: boolean) => {
      if (!value) return;
      onChange({ ...value, enable_second_agent: checked });
    },
    [value, onChange]
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
          <InfoTooltip text={t('tooltips.pipeline')} ruKey="pipeline" />
        </div>

        {enabled && (
          <div className="flex flex-col gap-base rounded-sm border border-border/60 p-base">
            {/* Template selector */}
            <div className="flex items-center gap-half">
              <label className="text-xs text-low w-20 shrink-0">
                {t('template')}
              </label>
              <Select
                value={config.name}
                onValueChange={(v) => onChange({ ...config, name: v })}
              >
                <SelectTrigger className="h-8 text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="Full Pipeline">
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
                  {STAGE_KEYS.map((stageId) => {
                    const stage = findStage(config, stageId);
                    if (!stage) return null;
                    const isReviewer = isReviewerStage(stageId);
                    const i18nKey = STAGE_I18N_KEY[stageId] ?? stageId;

                    return (
                      <tr
                        key={stageId}
                        className="border-b border-border/20 last:border-b-0"
                      >
                        {/* Stage name */}
                        <td className="py-1.5 pr-2">
                          <div className="flex items-center">
                            <span className="text-normal font-medium">
                              {t(`stages.${i18nKey}`)}
                            </span>
                            <InfoTooltip text={t(`tooltips.${i18nKey}`)} ruKey={i18nKey} />
                          </div>
                        </td>

                        {/* Agent selector */}
                        <td className="py-1.5 px-2">
                          <Select
                            value={stage.agent}
                            onValueChange={(v) =>
                              handleStageUpdate(stageId, {
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
                                handleStageUpdate(stageId, {
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
                            {isReviewer && stage.workflow_mode && (
                              <div className="flex items-center gap-1">
                                <span className="text-low text-[10px]">
                                  {t('reviewMode')}:
                                </span>
                                <Select
                                  value={stage.workflow_mode}
                                  onValueChange={(v) =>
                                    handleStageUpdate(stageId, {
                                      workflow_mode: v,
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
                                        ruKey="consensus"
                                      />
                                    </SelectItem>
                                    <SelectItem value="strict">
                                      {t('strict')}
                                      <InfoTooltip
                                        text={t('tooltips.strict')}
                                        ruKey="strict"
                                      />
                                    </SelectItem>
                                  </SelectContent>
                                </Select>
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

            {/* Second agent — global setting */}
            <div className="flex items-center gap-half pt-half">
              <Checkbox
                id="pipeline-second-agent"
                checked={config.enable_second_agent}
                onCheckedChange={(checked) => toggleSecondAgent(!!checked)}
              />
              <label
                htmlFor="pipeline-second-agent"
                className="text-xs text-normal cursor-pointer select-none"
              >
                {t('secondAgent')}
              </label>
              <InfoTooltip text={t('tooltips.secondAgent')} ruKey="secondAgent" />
            </div>
            {config.enable_second_agent && (
              <div className="flex items-center gap-half pl-5 pb-half">
                <Select
                  value={
                    config.stages.find((s) => s.escalate_agent)
                      ?.escalate_agent ?? 'codex'
                  }
                  onValueChange={(v) => {
                    // Update escalate_agent on all stages that have it
                    const newStages = config.stages.map((s) =>
                      s.escalate_agent != null
                        ? { ...s, escalate_agent: v as AgentId }
                        : s
                    );
                    onChange({ ...config, stages: newStages });
                  }}
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
                  value={
                    config.stages.find((s) => s.escalate_after_retries != null)
                      ?.escalate_after_retries ?? 3
                  }
                  onChange={(e) => {
                    const val = parseInt(e.target.value) || 3;
                    const newStages = config.stages.map((s) =>
                      s.escalate_after_retries != null
                        ? { ...s, escalate_after_retries: val }
                        : s
                    );
                    onChange({ ...config, stages: newStages });
                  }}
                  className="h-6 w-12 text-[10px] px-1"
                />
              </div>
            )}

            {/* Create PR checkbox */}
            <div className="flex items-center gap-half pt-half">
              <Checkbox
                id="pipeline-create-pr"
                checked={config.auto_create_pr}
                onCheckedChange={(checked) =>
                  onChange({ ...config, auto_create_pr: !!checked })
                }
              />
              <label
                htmlFor="pipeline-create-pr"
                className="text-xs text-normal cursor-pointer select-none"
              >
                {t('createPr')}
              </label>
              <InfoTooltip text={t('tooltips.createPr')} ruKey="createPr" />
            </div>
          </div>
        )}
      </div>
    </TooltipProvider>
  );
}
