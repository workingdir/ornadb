# 31. Worked reference database {#examples}

The reference database demonstrates book lending, stock transfers, exact values and resumable sensor ingestion. Its five modules run with local data and the intrinsic Orna environment. The source files are available in [`examples/reference/`](examples/reference/README.md).

## Project layout

```text
main.orna
library.orna
warehouse.orna
sensors.orna
values.orna
```

The root imports the four modules explicitly. Only files reachable from `main.orna` belong to the program.

```orna
use library;
use warehouse;
use sensors;
use values;

pub fn seed() {
    library.seed();
    warehouse.seed();
}

pub fn exercise() {
    library.lend("book-1", "reader-1");
    warehouse.transfer("north", "south", "pencil", 3);
}
```

## Lending: keys and assertions

A loan uses its book ID as its primary key, so at most one borrower can hold a given book. Table-owned assertions check local row content. The module assertion relates two tables and therefore has no implicit owner subject. Lending a missing book fails at commit; no invalid loan is published. After seed and exercise, only book-2 is returned by available. See [tables](#tables), [assertion validation](#tables), and [transactions](#execution).

```orna
pub table Book(id: Str) {
    title: Str,
    assert every(book => book.title != "");
}

pub table Loan(book_id: Str) {
    borrower: Str,
    assert every(loan => loan.borrower != "");
}

assert every(Loan, loan =>
    exists(Book, book => book.id == loan.book_id)
);

pub fn seed() {
    Book.insert({ id: "book-1", title: "The Night Garden" });
    Book.insert({ id: "book-2", title: "A Map of Small Things" });
}

pub fn lend(book_id: Str, borrower: Str) {
    Loan.insert({ book_id: book_id, borrower: borrower });
}

pub fn return_book(book_id: Str) {
    Loan.delete(book_id);
}

pub fn available() =
    Book | filter(book => !exists(Loan, loan => loan.book_id == book.id));
```

## Inventory: one atomic transfer

The composite key distinguishes stock at two locations. A transfer reads both rows in one activation, checks its preconditions, then updates both. From quantities 12 and 4, transferring 3 produces 9 and 7. A failure after the first tentative update still rolls back both changes. See [relation operators](#standard-library) and [activation transactions](#execution).

```orna
pub table Stock(location: Str, sku: Str) {
    quantity: Int,
    assert every(stock => stock.quantity >= 0);
}

pub fn seed() {
    Stock.insert({ location: "north", sku: "pencil", quantity: 12 });
    Stock.insert({ location: "south", sku: "pencil", quantity: 4 });
}

pub fn transfer(from_location: Str, to_location: Str, sku: Str, amount: Int) {
    assert amount > 0;
    assert from_location != to_location;
    let origin = Stock | filter(stock =>
        stock.location == from_location && stock.sku == sku
    ) | one();
    let destination = Stock | filter(stock =>
        stock.location == to_location && stock.sku == sku
    ) | one();
    assert origin.quantity >= amount;
    Stock.update((from_location, sku), { quantity: origin.quantity - amount });
    Stock.update((to_location, sku), { quantity: destination.quantity + amount });
}
```

## Sensors: resumable finite input

The sample type is a nominal value; Reading is persistent data. The list source has a complete built-in identity and replay contract. Three successful callbacks produce three rows and a next-item checkpoint of 3. Restarting the same consumer resumes at exhaustion. It does not repeatedly insert the same rows. See [streams](#streams), [checkpoints](#checkpoints), and [finite-source semantics](#standard-library).

```orna
pub type Sample {
    pub sensor: Str,
    pub sequence: Int,
    pub value: Decimal,
}

pub table Reading(sensor: Str, sequence: Int) {
    value: Decimal,
    assert every(reading => reading.sequence >= 0);
}

pub fn input() = Stream.from_list([
    Sample { sensor: "greenhouse", sequence: 0, value: 18.25 },
    Sample { sensor: "greenhouse", sequence: 1, value: 18.50 },
    Sample { sensor: "greenhouse", sequence: 2, value: 18.75 },
], source_identity: "example:sensors:v1");

pub fn ingest() {
    input() | for_each(sample => {
        Reading.insert({
            sensor: sample.sensor,
            sequence: sample.sequence,
            value: sample.value,
        });
    });
}
```

## Values: refinement, variants and option

Score has an Int representation with always-enforced bounds. Availability is an ordinary enum with a payload-bearing variant. The Option example deliberately shows both Some and null. Neither enum branching nor optional values are an exception-catching mechanism. See [types](#types), [case expressions](#expressions), and [failure recovery](#expressions).

```orna
pub type Score = Int {
    assert >= 0;
    assert <= 100;
}

pub enum Availability {
    ready,
    waiting { reason: Str },
}

pub fn describe(value: Availability): Str = case value {
    Availability.ready: "ready",
    Availability.waiting { reason }: "waiting: {reason}",
};

pub fn optional_name(value: Str?): Str = case value {
    Some(name): name,
    null: "anonymous",
};

pub fn add(left: Int, right: Int): Int = left + right;
```

## Interactive inspection

The following are individual REPL inputs after loading the reference database. They are not additional module-level executable statements.

```text
library.available() | map(book => book.title)

sys.catalog.objects | map(object => object.qualified_name)

let function = sys.resolve_function("values.add");
sys.invoke(function, { left: 2, right: 3 }, as: Int)
```

The invocation returns 5. Its argument record is checked by the explicitly reflective ArgumentMap boundary; it does not permit arbitrary implicit boxing elsewhere. To inspect history, resolve a snapshot explicitly before using it:

```orna
let previous = sys.snapshot("HEAD~1");
library.Book.as_of(previous)
```

This reads historical data under the current query's code. A whole-program historical evaluation uses a database snapshot object instead; see [historical evaluation](#relations). A repository without an earlier commit fails the selector rather than returning an empty table.

## Cancellation without changing result types

A synchronous helper can wait, catch only an ordinary timeout, and await again. Both branches return the same `sys.InvocationResult<Int>` type:

```orna
pub fn wait_for_int(job: sys.InvocationHandle<Int>): sys.InvocationResult<Int> =
    sys.await(job, timeout: 1.s) |? (failure => {
        if failure.code == "sys.invoke.await_timeout" {
            sys.await(job)
        } else {
            fail(failure)
        }
    });
```

The helper is illustrative source requiring a live handle supplied by its caller. It does not change that handle's owner. An unhandled error that ends the owner triggers child cancellation for that separate reason; a timed-out wait alone does not.

## Testing these examples

Each example case runs in a disposable database with empty tables. The distributed expectations specify initial state, inputs, exact resulting rows and failure outcomes. Parser acceptance is separate from resolver/type/effect checking, and both are separate from execution. A conforming implementation must run all three stages before claiming the example project passes.

