import { describe, expect, it } from "vitest";
import { basename, dirname, fmtBytes, fmtDuration, fmtNum } from "../format";

describe("fmtDuration", () => {
  it.each([
    [null, "–"],
    [undefined, "–"],
    [NaN, "–"],
    [Infinity, "–"],
    [-Infinity, "–"],
    [-5, "0 s"],
    [0, "0 s"],
    [0.4, "0 s"],
    [59.4, "59 s"],
    [59.5, "1 min 00 s"],
    [61, "1 min 01 s"],
    [600, "10 min 00 s"],
    [3599, "59 min 59 s"],
    [3600, "1 h 00 min"],
    [3661, "1 h 01 min"],
    [5 * 3600 + 7 * 60 + 59, "5 h 07 min"],
    [25 * 3600 + 60, "25 h 01 min"],
  ])("%s s → %s", (input, out) => {
    expect(fmtDuration(input as number | null | undefined)).toBe(out);
  });
});

describe("fmtBytes", () => {
  it.each([
    [null, "–"],
    [undefined, "–"],
    [0, "0 B"],
    [1, "1 B"],
    [1023, "1023 B"],
    [1024, "1.0 KB"],
    [1536, "1.5 KB"],
    [1024 * 1024 - 1, "1024.0 KB"],
    [1024 * 1024, "1.0 MB"],
    [12_345_678, "11.8 MB"],
    [4_000_000_000, "3.7 GB"],
    [1024 ** 4, "1.0 TB"],
    [1024 ** 5, "1024.0 TB"],
  ])("%s → %s", (input, out) => {
    expect(fmtBytes(input as number | null | undefined)).toBe(out);
  });
});

describe("fmtNum", () => {
  it("rounds to two digits by default", () => expect(fmtNum(3.14159)).toBe("3.14"));
  it("honours the digit count", () => {
    expect(fmtNum(3.14159, 0)).toBe("3");
    expect(fmtNum(2.5, 1)).toBe("2.5");
    expect(fmtNum(0.0004, 3)).toBe("0.000");
  });
  it("pads with zeros", () => expect(fmtNum(7, 2)).toBe("7.00"));
  it("keeps the sign", () => expect(fmtNum(-1.25, 1)).toBe("-1.3"));
  it("shows a dash for non-finite values", () => {
    expect(fmtNum(NaN)).toBe("–");
    expect(fmtNum(Infinity)).toBe("–");
    expect(fmtNum(-Infinity, 1)).toBe("–");
  });
});

describe("basename / dirname", () => {
  it.each([
    ["C:\\Users\\mika\\Desktop\\benchy.stl", "benchy.stl", "C:\\Users\\mika\\Desktop"],
    ["/home/mika/models/benchy.stl", "benchy.stl", "/home/mika/models"],
    ["/local/minis/knight.goo", "knight.goo", "/local/minis"],
    ["C:/mixed\\path/part.3mf", "part.3mf", "C:/mixed\\path"],
    ["plate.orproj", "plate.orproj", ""],
    ["/root.goo", "root.goo", ""],
  ])("%s", (p, base, dir) => {
    expect(basename(p)).toBe(base);
    expect(dirname(p)).toBe(dir);
  });
});
