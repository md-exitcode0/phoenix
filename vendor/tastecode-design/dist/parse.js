function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function record(value, field) {
  if (!isRecord(value)) throw new Error(`${field} must be an object`);
  return value;
}
function optionalRecord(value) {
  return isRecord(value) ? value : void 0;
}
function string(value, field) {
  if (typeof value !== "string" || value.trim() === "") {
    throw new Error(`${field} must be a non-empty string`);
  }
  return value;
}
function optionalString(value, field) {
  if (value === void 0) return void 0;
  return string(value, field);
}
function strings(value, field) {
  if (!Array.isArray(value) || !value.every((entry) => typeof entry === "string" && entry.trim())) {
    throw new Error(`${field} must be a string array`);
  }
  return value;
}
function stringsAllowEmpty(value, field) {
  if (!Array.isArray(value) || !value.every((entry) => typeof entry === "string")) {
    throw new Error(`${field} must be a string array`);
  }
  return value;
}
function list(value, field) {
  if (!Array.isArray(value)) throw new Error(`${field} must be an array`);
  return value;
}
function array(value, field) {
  if (!Array.isArray(value) || value.length === 0) {
    throw new Error(`${field} must be a non-empty array`);
  }
  return value;
}
function integer(value, field, minimum, maximum) {
  const parsed = boundedInteger(value, minimum, maximum);
  if (parsed === void 0) {
    throw new Error(`${field} must be an integer from ${minimum} to ${maximum}`);
  }
  return parsed;
}
function boundedInteger(value, minimum, maximum) {
  return typeof value === "number" && Number.isInteger(value) && value >= minimum && value <= maximum ? value : void 0;
}
function fontWeights(value, field) {
  if (!Array.isArray(value) || value.length === 0) {
    throw new Error(`${field} must contain font weights between 1 and 1000`);
  }
  const weights = value.map(
    (entry) => typeof entry === "string" && /^\d{1,4}$/u.test(entry) ? Number(entry) : entry
  );
  if (!weights.every(
    (weight) => typeof weight === "number" && Number.isInteger(weight) && weight >= 1 && weight <= 1e3
  )) {
    throw new Error(`${field} must contain font weights between 1 and 1000`);
  }
  return weights;
}
function member(value, values, field) {
  if (typeof value !== "string" || !isMember(value, values)) {
    throw new Error(`${field} must be one of ${values.join(", ")}`);
  }
  return value;
}
function isMember(value, values) {
  return values.some((candidate) => candidate === value);
}
export {
  array,
  boundedInteger,
  fontWeights,
  integer,
  list,
  member,
  optionalRecord,
  optionalString,
  record,
  string,
  strings,
  stringsAllowEmpty
};
