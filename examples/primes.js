// Rust engine `--cmd` example in JavaScript (Node): count primes in [START, END).
//   ./kit rs run --n 1e7 --chunk 1e6 --cmd "node examples/primes.js" | awk '{s+=$1} END {print s}'
const [start, end] = process.argv.slice(2).map(Number);

function isPrime(n) {
  if (n < 2) return false;
  if (n % 2 === 0) return n === 2;
  for (let i = 3; i * i <= n; i += 2) if (n % i === 0) return false;
  return true;
}

let count = 0;
for (let n = start; n < end; n++) if (isPrime(n)) count++;
console.log(count);
