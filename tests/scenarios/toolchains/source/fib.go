package main
import "fmt"
func main(){ var a,b uint64 =0,1; for i:=0;i<50;i++{ a,b=b,a+b }; fmt.Printf("R=%d\n", a) }
