// Seed a dev database. Run with:
//   docker exec -i viti-mongo mongosh --quiet < scripts/seed.js
const db = connect("mongodb://localhost:27017/viti_test");
db.dropDatabase();

const first = ["Ann", "Bob", "Chen", "Dita", "Eyal", "Femi", "Gus", "Hana", "Ivo", "Jun"];
const last = ["Adams", "Baker", "Cole", "Diaz", "Egan", "Fox", "Gray", "Hill", "Ito", "Jain"];
const tags = ["alpha", "beta", "gamma", "delta", "vip", "trial", "churned"];
const people = [];
for (let i = 0; i < 10000; i++) {
  const doc = {
    name: `${first[i % 10]} ${last[Math.floor(i / 10) % 10]}`,
    age: 18 + (i * 7) % 60,
    email: `user${i}@example.com`,
    active: i % 3 !== 0,
    score: Math.round(Math.random() * 10000) / 100,
    balance: NumberLong(i * 1000),
    tags: [tags[i % 7], tags[(i * 3) % 7]],
    address: { city: ["Oslo", "Lima", "Kyoto", "Cairo", "Perth"][i % 5], zip: String(10000 + i), geo: [10 + (i % 50) / 10, 60 - (i % 30) / 10] },
    created: new Date(Date.now() - i * 3600 * 1000),
    notes: i % 50 === 0 ? null : `note ${i}`,
    nested: { level1: { level2: { level3: { value: i } } } },
  };
  if (i % 100 === 0) doc.decimal = NumberDecimal("12345.6789");
  if (i % 250 === 0) doc.pattern = /^user\d+/i;
  if (i % 17 === 0) delete doc.notes;
  people.push(doc);
}
db.people.insertMany(people);
db.people.createIndex({ email: 1 }, { unique: true });
db.people.createIndex({ age: 1, name: 1 });
db.people.createIndex({ "address.geo": "2d" });
db.people.createIndex({ name: "text" });

db.createCollection("orders", { timeseries: { timeField: "ts", metaField: "meta", granularity: "hours" } });
const orders = [];
for (let i = 0; i < 2000; i++) {
  orders.push({ ts: new Date(Date.now() - i * 7200 * 1000), meta: { region: ["eu", "us", "apac"][i % 3] }, amount: (i % 97) * 1.5, items: (i % 5) + 1 });
}
db.orders.insertMany(orders);

db.createCollection("adults", { viewOn: "people", pipeline: [{ $match: { age: { $gte: 18 } } }, { $project: { name: 1, age: 1 } }] });

db.createCollection("validated", {
  validator: { $jsonSchema: { bsonType: "object", required: ["sku", "qty"], properties: { sku: { bsonType: "string" }, qty: { bsonType: "int", minimum: 0 } } } },
  validationAction: "error",
  validationLevel: "strict",
});
db.validated.insertMany([{ sku: "A-1", qty: NumberInt(3) }, { sku: "B-2", qty: NumberInt(0) }]);

db.empty.insertOne({ placeholder: true });
db.empty.deleteMany({});

print(`seeded: people=${db.people.countDocuments()} orders=${db.orders.countDocuments()} validated=${db.validated.countDocuments()}`);
