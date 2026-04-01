import { parseArgs } from 'node:util';
import { readdirSync, statSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { chromium } from 'playwright';

// ── Types ──────────────────────────────────────────────────────────────

type AccountStatus =
  | 'logged_in'
  | 'login_required'
  | 'challenge'
  | 'error'
  | 'unknown';

interface ProfileInfo {
  name: string;
  path: string;
}

interface CheckResult {
  profile_name: string;
  status: AccountStatus;
  reason: string;
  checked_at: string;
}

// ── Profile Discovery ──────────────────────────────────────────────────

function discoverProfiles(rootDir: string): ProfileInfo[] {
  const entries = readdirSync(rootDir);
  const profiles: ProfileInfo[] = [];

  for (const entry of entries) {
    if (entry.startsWith('.')) continue;
    const fullPath = join(rootDir, entry);
    try {
      if (statSync(fullPath).isDirectory()) {
        profiles.push({ name: entry, path: fullPath });
      }
    } catch {
      // Skip entries we can't stat
    }
  }

  return profiles.sort((a, b) => a.name.localeCompare(b.name));
}

// ── Status Classification ──────────────────────────────────────────────

const CHALLENGE_PATTERNS = [
  'Verify your identity',
  'Confirm your phone',
  'suspicious activity',
  'arkose',
];

async function checkProfile(
  profilePath: string,
  profileName: string,
  options: { timeout: number; headless: boolean }
): Promise<CheckResult> {
  const checkedAt = new Date().toISOString();
  let context;

  try {
    context = await chromium.launchPersistentContext(profilePath, {
      headless: options.headless,
      args: ['--disable-blink-features=AutomationControlled'],
    });

    const page = context.pages()[0] || (await context.newPage());

    await page.goto('https://x.com/home', {
      waitUntil: 'domcontentloaded',
      timeout: options.timeout,
    });

    // Let redirects settle (don't fail on timeout)
    await page
      .waitForLoadState('networkidle', { timeout: 5000 })
      .catch(() => {});

    const url = page.url();

    // URL-based checks
    if (url.includes('/login') || url.includes('/i/flow/login')) {
      return {
        profile_name: profileName,
        status: 'login_required',
        reason: `Redirected to login page: ${url}`,
        checked_at: checkedAt,
      };
    }

    if (
      url.includes('/account/access') ||
      url.includes('/i/flow/consent')
    ) {
      return {
        profile_name: profileName,
        status: 'challenge',
        reason: `Account challenge page: ${url}`,
        checked_at: checkedAt,
      };
    }

    // Content-based checks: logged in
    const homeIndicator = page.locator(
      '[data-testid="primaryColumn"], [data-testid="AppTabBar_Home_Link"]'
    );
    try {
      await homeIndicator.first().waitFor({ timeout: 3000 });
      return {
        profile_name: profileName,
        status: 'logged_in',
        reason: 'Home timeline detected',
        checked_at: checkedAt,
      };
    } catch {
      // Not logged in via this indicator
    }

    // Content-based checks: login form
    const loginIndicator = page.locator(
      '[data-testid="LoginForm_Login_Button"], input[autocomplete="username"]'
    );
    try {
      await loginIndicator.first().waitFor({ timeout: 3000 });
      return {
        profile_name: profileName,
        status: 'login_required',
        reason: 'Login form detected',
        checked_at: checkedAt,
      };
    } catch {
      // No login form found
    }

    // Content-based checks: challenge/captcha
    try {
      const bodyText = await page
        .locator('body')
        .innerText({ timeout: 3000 });
      for (const pattern of CHALLENGE_PATTERNS) {
        if (bodyText.includes(pattern)) {
          return {
            profile_name: profileName,
            status: 'challenge',
            reason: `Challenge content detected: ${pattern}`,
            checked_at: checkedAt,
          };
        }
      }
    } catch {
      // Could not read body text
    }

    // Fallback
    return {
      profile_name: profileName,
      status: 'unknown',
      reason: `Could not determine status. Final URL: ${url}`,
      checked_at: checkedAt,
    };
  } catch (err) {
    return {
      profile_name: profileName,
      status: 'error',
      reason: `Error: ${err instanceof Error ? err.message : String(err)}`,
      checked_at: checkedAt,
    };
  } finally {
    if (context) {
      await context.close().catch(() => {});
    }
  }
}

// ── CSV Writing ────────────────────────────────────────────────────────

function escapeCsvField(field: string): string {
  if (
    field.includes(',') ||
    field.includes('"') ||
    field.includes('\n')
  ) {
    return `"${field.replace(/"/g, '""')}"`;
  }
  return field;
}

function writeCsv(results: CheckResult[], outputPath: string): void {
  const header = 'profile_name,status,reason,checked_at';
  const rows = results.map(
    (r) =>
      [r.profile_name, r.status, r.reason, r.checked_at]
        .map(escapeCsvField)
        .join(',')
  );
  writeFileSync(outputPath, [header, ...rows].join('\n') + '\n', 'utf-8');
}

// ── Main ───────────────────────────────────────────────────────────────

const USAGE = `Usage: check-x-status [options]

Options:
  --profiles-root <path>   Path to directory containing browser profile subdirectories (required)
  --output <path>          Path to output CSV file (default: ./x-status-results.csv)
  --timeout <ms>           Per-page navigation timeout in ms (default: 30000)
  --headless               Run browser headless (default: true)
  --no-headless            Show the browser window
  --help                   Print this help message`;

async function main(): Promise<void> {
  // Filter leading '--' that pnpm/npm pass through when using `pnpm run script -- --flag`
  const rawArgs = process.argv.slice(2);
  const args = rawArgs[0] === '--' ? rawArgs.slice(1) : rawArgs;

  const { values } = parseArgs({
    args,
    options: {
      'profiles-root': { type: 'string' },
      output: { type: 'string', default: './x-status-results.csv' },
      timeout: { type: 'string', default: '30000' },
      headless: { type: 'boolean', default: true },
      help: { type: 'boolean', default: false },
    },
    strict: true,
  });

  if (values.help) {
    console.log(USAGE);
    process.exit(0);
  }

  const profilesRoot = values['profiles-root'];
  if (!profilesRoot) {
    console.error(
      'Error: --profiles-root is required.\n\n' + USAGE
    );
    process.exit(1);
  }

  const resolvedRoot = resolve(profilesRoot);
  let rootStat;
  try {
    rootStat = statSync(resolvedRoot);
  } catch {
    console.error(
      `Error: --profiles-root path does not exist: ${resolvedRoot}`
    );
    process.exit(1);
  }
  if (!rootStat.isDirectory()) {
    console.error(
      `Error: --profiles-root is not a directory: ${resolvedRoot}`
    );
    process.exit(1);
  }

  const outputPath = resolve(values.output!);
  const timeout = parseInt(values.timeout!, 10);
  if (isNaN(timeout) || timeout <= 0) {
    console.error('Error: --timeout must be a positive integer.');
    process.exit(1);
  }
  const headless = values.headless!;

  const profiles = discoverProfiles(resolvedRoot);
  if (profiles.length === 0) {
    console.error(
      `No profile subdirectories found in: ${resolvedRoot}`
    );
    process.exit(1);
  }

  console.log(
    `Found ${profiles.length} profile(s) in ${resolvedRoot}`
  );
  console.log(
    `Settings: headless=${headless}, timeout=${timeout}ms, output=${outputPath}\n`
  );

  const results: CheckResult[] = [];

  for (let i = 0; i < profiles.length; i++) {
    const profile = profiles[i];
    console.log(
      `Checking profile [${i + 1}/${profiles.length}]: ${profile.name}...`
    );

    let result: CheckResult;
    try {
      result = await checkProfile(profile.path, profile.name, {
        timeout,
        headless,
      });
    } catch (err) {
      result = {
        profile_name: profile.name,
        status: 'error',
        reason: `Error: ${err instanceof Error ? err.message : String(err)}`,
        checked_at: new Date().toISOString(),
      };
    }

    results.push(result);
    console.log(`  → ${result.status} (${result.reason})`);
  }

  writeCsv(results, outputPath);
  console.log(
    `\nDone. ${results.length} profiles checked. Results saved to ${outputPath}`
  );
}

main().catch((err) => {
  console.error(
    `Error: ${err instanceof Error ? err.message : String(err)}`
  );
  process.exit(1);
});
