// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setHomeDir } from "../../../shared/lib/paths";
import { loadRecents, rememberProject } from "./recents";
import {
  noticeReservedProjects,
  openPickedFolders,
  RESERVED_PROJECT_NAME_ERROR,
  reservedProjectNameError,
} from "./reservedProjectName";

const HOME = "/Users/u";

beforeEach(() => {
  localStorage.clear();
  setHomeDir(HOME);
});
afterEach(() => setHomeDir(undefined));

const storedPaths = () => loadRecents().map((project) => project.path);

describe("reservedProjectNameError", () => {
  it.each(["MonoCode", "monocode", " MonoCode ", "MONOCODE"])(
    "refuses a folder named %j outside the home folder",
    (name) => {
      expect(reservedProjectNameError(`/tmp/x/${name}`, HOME)).toBe(
        RESERVED_PROJECT_NAME_ERROR,
      );
    },
  );

  it("refuses a folder named MonoCode given with a trailing slash", () => {
    expect(reservedProjectNameError("/tmp/x/MonoCode/", HOME)).toBe(
      RESERVED_PROJECT_NAME_ERROR,
    );
  });

  it.each([`${HOME}/MonoCode`, `${HOME}/MonoCode/`])(
    "allows the Giga workspace itself at %s",
    (path) => {
      expect(reservedProjectNameError(path, HOME)).toBeNull();
    },
  );

  it.each(["/tmp/x/MonoCodeX", "/tmp/x/my-monocode"])(
    "allows a name that only contains MonoCode: %s",
    (path) => {
      expect(reservedProjectNameError(path, HOME)).toBeNull();
    },
  );

  it("refuses a folder named MonoCode on another machine, even at the home path", () => {
    expect(
      reservedProjectNameError(`remote://env${HOME}/MonoCode`, HOME),
    ).toBe(RESERVED_PROJECT_NAME_ERROR);
  });
});

describe("Open folder…", () => {
  it("opens the allowed folders and refuses one named MonoCode", async () => {
    rememberProject("/code/app");
    const before = storedPaths();
    const refuse = vi.fn();
    await openPickedFolders(
      async () => ["/tmp/x/MonoCode", "/code/site"],
      (paths) => paths.forEach(rememberProject),
      refuse,
    );
    expect(storedPaths()).toEqual(["/code/site", ...before]);
    expect(refuse).toHaveBeenCalledExactlyOnceWith(RESERVED_PROJECT_NAME_ERROR);
  });

  it("opens the Giga workspace without a refusal", async () => {
    const refuse = vi.fn();
    await openPickedFolders(
      async () => [`${HOME}/MonoCode`],
      (paths) => paths.forEach(rememberProject),
      refuse,
    );
    expect(storedPaths()).toEqual([`${HOME}/MonoCode`]);
    expect(refuse).not.toHaveBeenCalled();
  });
});

describe("a MonoCode project added before the name was reserved", () => {
  it("stays in the list and is noticed exactly once", () => {
    rememberProject("/tmp/x/MonoCode");
    rememberProject(`${HOME}/MonoCode`);
    rememberProject("/code/app");
    const before = storedPaths();
    const show = vi.fn();

    noticeReservedProjects(show);
    noticeReservedProjects(show);

    expect(show).toHaveBeenCalledOnce();
    expect(show.mock.calls[0][0]).toContain("/tmp/x/MonoCode");
    expect(show.mock.calls[0][0]).not.toContain(`${HOME}/MonoCode`);
    expect(storedPaths()).toEqual(before);
  });
});
