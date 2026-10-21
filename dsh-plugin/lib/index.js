/**
 * Resolve this package's bundled Skill root from the DSH profile.
 *
 * The profile's Loader passes its `baseUrl`; resolving through
 * `createRequire(baseUrl)` finds the copy pnpm actually installed for that
 * profile, which is the only correct answer when several profiles (or a global
 * store) hold different versions. Concatenating a path onto `baseUrl` instead
 * would break under pnpm's hoisted layout.
 *
 * @param {string | undefined} profileBaseUrl Loader base URL of the DSH profile.
 * @returns {string} absolute path to this package's `skills/` directory.
 */
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';

export const name = 'auto-memory-dsh';
export const PACKAGE_NAME = 'auto-memory-dsh';

/**
 * @param {string | undefined} profileBaseUrl
 * @returns {string}
 */
export function resolveSkillRoot(profileBaseUrl) {
  if (!profileBaseUrl) {
    throw new Error('auto-memory-dsh: missing DSH profile baseUrl for package resolution');
  }
  let manifestPath;
  try {
    manifestPath = createRequire(profileBaseUrl).resolve(`${PACKAGE_NAME}/package.json`);
  } catch (error) {
    throw new Error(
      `auto-memory-dsh: cannot resolve ${PACKAGE_NAME}/package.json from the DSH profile`,
      { cause: error },
    );
  }
  return join(dirname(manifestPath), 'skills');
}
